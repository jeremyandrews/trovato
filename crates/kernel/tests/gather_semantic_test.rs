#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for the pgvector-backed `SemanticSimilarity` gather
//! operator (read path) and `tap_item_update_index` (write path), wired in
//! PF-4 / D-5.
//!
//! These exercise the **gather** and **item-lifecycle** layers — above the
//! storage layer covered by `vector_store_test.rs`. The query-embedding HTTP
//! call requires a live embedding provider (and a non-local base URL, which
//! `validate_base_url` enforces for SSRF), so the full embed→search chain is
//! not exercised end to end here. Its halves are covered by unit tests
//! (`parse_embedding_vector`, the query-builder `id IN`/`FALSE` rewrite) plus
//! the pgvector-gated store→gather composition test below.

mod common;

use common::{run_test, shared_app};
use std::collections::HashMap;
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter, QuerySort, SortDirection,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, UpdateItem};
use trovato_kernel::services::vector_store::{PgVectorStore, VectorStore};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Minimal table display for tests.
fn test_display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::Table,
        items_per_page: 25,
        pager: PagerConfig {
            enabled: true,
            style: PagerStyle::Full,
            show_count: true,
        },
        empty_text: None,
        header: None,
        footer: None,
        canonical_url: None,
        routes: Vec::new(),
        feed: None,
    }
}

/// A query context whose viewer is a site administrator. Gather now enforces
/// item access (FR-8 Story 3.4), so these semantic/rewrite tests — which assert
/// on query *mechanics*, not on the access filter — run as an admin that
/// bypasses item access. The access filter has its own suite in
/// `gather_access_test.rs`.
fn admin_ctx() -> QueryContext {
    QueryContext {
        viewer: Some(UserContext::authenticated(
            Uuid::nil(),
            vec!["administer site".to_string()],
        )),
        ..QueryContext::default()
    }
}

/// Collect the `id` strings from a gather result's JSON rows.
fn result_ids(items: &[serde_json::Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect()
}

/// Read path — degradation: a `semantic_similarity` filter with no embedding
/// provider configured must yield an empty result set (no-match), NOT widen to
/// all items and NOT error. Proves the operator path runs end to end through
/// GatherService and falls back to the FALSE safety net rather than the
/// pre-PF-4 behavior of silently returning nothing-via-stub *or* (had the
/// rewrite been naive) widening.
#[test]
fn semantic_filter_without_provider_returns_empty_not_all() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_items().await;

        // Conferences exist, so a *widened* (dropped) filter would return > 0,
        // which makes the `total == 0` assertion meaningful.
        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("conference".to_string()),
            filters: vec![QueryFilter {
                field: "id".to_string(),
                operator: FilterOperator::SemanticSimilarity,
                value: FilterValue::String("rust web assembly conference".to_string()),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            }],
            sorts: vec![],
            stage_aware: true,
            ..Default::default()
        };

        let ctx = admin_ctx();
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await
            .expect("gather must not error on semantic degradation");

        assert_eq!(
            result.total, 0,
            "semantic filter with no provider must be no-match, not widened"
        );
        assert!(result.items.is_empty());
    });
}

/// Read path — rewrite target: the `id IN (...)` predicate the semantic
/// pre-pass produces must select exactly the listed items against the real
/// `uuid` `id` column (proving the UUID-binding fix; binding the canonical
/// strings as text would raise `operator does not exist: uuid = text`).
/// Independent of pgvector.
#[test]
fn id_in_rewrite_selects_only_listed_items() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_items().await;

        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM item WHERE type = 'conference' AND stage_id = $1 ORDER BY title LIMIT 2",
        )
        .bind(LIVE_STAGE_ID)
        .fetch_all(&app.db)
        .await
        .expect("failed to load conference ids");
        assert_eq!(ids.len(), 2, "need at least two seeded conferences");

        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("conference".to_string()),
            filters: vec![QueryFilter {
                field: "id".to_string(),
                operator: FilterOperator::In,
                value: FilterValue::List(ids.iter().copied().map(FilterValue::Uuid).collect()),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            }],
            sorts: vec![QuerySort {
                field: "title".to_string(),
                direction: SortDirection::Asc,
                nulls: None,
            }],
            stage_aware: true,
            ..Default::default()
        };

        let ctx = admin_ctx();
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await
            .expect("id IN gather should execute");

        assert_eq!(result.total, 2, "exactly the two listed ids should match");
        let returned = result_ids(&result.items);
        for id in &ids {
            assert!(
                returned.iter().any(|r| r == &id.to_string()),
                "expected {id} in results {returned:?}"
            );
        }
    });
}

/// Write path — `tap_item_update_index` + best-effort embedding run on BOTH
/// create and update without failing the save, even when no embedding provider
/// or pgvector is available (the tap fires; kernel embedding generation is
/// skipped). A regression guard that the index hook is wired into both
/// lifecycle methods and degrades cleanly.
#[test]
fn item_create_and_update_run_index_best_effort() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let user = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let created = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: "PF-4 Index Smoke".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_description": { "value": "A conference about vectors." }
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("PF-4 test".to_string()),
                },
                &user,
            )
            .await
            .expect("create should succeed with index best-effort");

        let updated = app
            .state
            .items()
            .update(
                created.id,
                UpdateItem {
                    title: Some("PF-4 Index Smoke (edited)".to_string()),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_description": { "value": "Updated description." }
                    })),
                    log: Some("PF-4 test update".to_string()),
                },
                &user,
            )
            .await
            .expect("update should succeed with index best-effort")
            .expect("item should exist for update");

        assert_eq!(updated.title, "PF-4 Index Smoke (edited)");

        // Cleanup.
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(created.id)
            .execute(&app.db)
            .await
            .ok();
    });
}

/// Read path — store→gather composition, gated on pgvector. Seeds an embedding
/// directly via the store (bypassing the provider-dependent embed call), then
/// proves a `similarity_search` result feeds an `id IN (...)` gather that
/// returns the item. Skips with a message when pgvector is unavailable.
#[test]
fn embedding_search_feeds_gather_when_pgvector_available() {
    run_test(async {
        let app = shared_app().await;
        let store = PgVectorStore::new(app.db.clone()).await;
        if !store.is_available().await {
            println!("pgvector unavailable — skipping store→gather composition test");
            return;
        }
        app.ensure_conference_items().await;

        let target: Uuid = sqlx::query_scalar(
            "SELECT id FROM item WHERE type = 'conference' AND stage_id = $1 ORDER BY title LIMIT 1",
        )
        .bind(LIVE_STAGE_ID)
        .fetch_one(&app.db)
        .await
        .unwrap();

        let model = "pf4-test-model";
        let embedding = vec![0.10_f32, 0.20, 0.30, 0.40];
        store
            .store_embedding(target, "_content", model, &embedding)
            .await
            .expect("store embedding");

        let hits = store
            .similarity_search(&embedding, model, 100)
            .await
            .expect("similarity search");
        assert!(
            hits.iter().any(|h| h.item_id == target),
            "stored embedding should be found"
        );

        let ids: Vec<Uuid> = hits.into_iter().map(|h| h.item_id).collect();
        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("conference".to_string()),
            filters: vec![QueryFilter {
                field: "id".to_string(),
                operator: FilterOperator::In,
                value: FilterValue::List(ids.into_iter().map(FilterValue::Uuid).collect()),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            }],
            sorts: vec![],
            stage_aware: true,
            ..Default::default()
        };
        let ctx = admin_ctx();
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await
            .expect("gather should execute");

        let returned = result_ids(&result.items);
        assert!(
            returned.iter().any(|r| r == &target.to_string()),
            "target item should be returned via the id-set gather"
        );

        store.delete_embeddings(target).await.ok();
    });
}

/// Read path — PF-4.1 Task 1: a semantic gather with **no explicit sort**
/// returns items in descending similarity (closest first), and an explicit
/// gather sort **overrides** that relevance order. Gated on pgvector.
///
/// The query-embedding HTTP call still needs a live provider, so the ranked
/// order is derived from a real `similarity_search` over directly-seeded
/// embeddings; the gather is then driven with that ranked id set plus the
/// `relevance_order` the semantic pre-pass would have recorded. This proves
/// the `ORDER BY array_position(...)` composes through the full gather pipeline
/// and DB exactly as the live path would produce it.
#[test]
fn semantic_relevance_order_default_and_sort_override() {
    run_test(async {
        let app = shared_app().await;
        let store = PgVectorStore::new(app.db.clone()).await;
        if !store.is_available().await {
            println!("pgvector unavailable — skipping relevance-order test");
            return;
        }
        app.ensure_conference_type().await;

        let user = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        // Three items whose alphabetical title order (Alpha, Mike, Zeta)
        // deliberately differs from their relevance order so the two
        // assertions below can't both pass by coincidence.
        let titles = [
            "Zeta Relevance Conf",
            "Mike Relevance Conf",
            "Alpha Relevance Conf",
        ];
        let mut item_ids = Vec::new();
        for title in titles {
            let created = app
                .state
                .items()
                .create(
                    CreateItem {
                        item_type: "conference".to_string(),
                        title: title.to_string(),
                        author_id: Uuid::nil(),
                        status: Some(1),
                        promote: Some(0),
                        sticky: Some(0),
                        fields: Some(serde_json::json!({})),
                        stage_id: Some(LIVE_STAGE_ID),
                        language: Some("en".to_string()),
                        log: None,
                    },
                    &user,
                )
                .await
                .expect("create conference item");
            item_ids.push(created.id);
        }
        let (zeta, mike, alpha) = (item_ids[0], item_ids[1], item_ids[2]);

        // Seed embeddings so the query vector [1,0,0,0] ranks Zeta closest,
        // then Mike, then Alpha (ascending cosine distance).
        let model = "pf41-relevance-model";
        let q = vec![1.0_f32, 0.0, 0.0, 0.0];
        for (id, vector) in [
            (zeta, vec![1.0_f32, 0.0, 0.0, 0.0]),
            (mike, vec![0.5_f32, 0.5, 0.0, 0.0]),
            (alpha, vec![0.0_f32, 1.0, 0.0, 0.0]),
        ] {
            store
                .store_embedding(id, "_content", model, &vector)
                .await
                .expect("store embedding");
        }

        // The ranked order the semantic pre-pass would produce.
        let hits = store
            .similarity_search(&q, model, 100)
            .await
            .expect("similarity search");
        let ranked: Vec<Uuid> = hits.into_iter().map(|h| h.item_id).collect();
        assert_eq!(
            ranked,
            vec![zeta, mike, alpha],
            "similarity_search should rank closest-first"
        );

        // Unsorted gather: relevance order drives the result order.
        let unsorted = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("conference".to_string()),
            filters: vec![QueryFilter {
                field: "id".to_string(),
                operator: FilterOperator::In,
                value: FilterValue::List(ranked.iter().copied().map(FilterValue::Uuid).collect()),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            }],
            sorts: vec![],
            relevance_order: Some(ranked.clone()),
            stage_aware: true,
            ..Default::default()
        };
        let ctx = admin_ctx();
        let result = app
            .state
            .gather()
            .execute_definition(
                &unsorted,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await
            .expect("unsorted relevance gather should execute");
        let returned = result_ids(&result.items);
        assert_eq!(
            returned,
            ranked.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
            "unsorted semantic gather must return most-similar-first"
        );

        // Explicit sort by title: alphabetical order wins over relevance.
        let sorted = QueryDefinition {
            sorts: vec![QuerySort {
                field: "title".to_string(),
                direction: SortDirection::Asc,
                nulls: None,
            }],
            ..unsorted.clone()
        };
        let result = app
            .state
            .gather()
            .execute_definition(
                &sorted,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await
            .expect("sorted relevance gather should execute");
        let returned = result_ids(&result.items);
        assert_eq!(
            returned,
            vec![alpha.to_string(), mike.to_string(), zeta.to_string()],
            "explicit title sort must override relevance order"
        );

        // Cleanup.
        for id in &item_ids {
            store.delete_embeddings(*id).await.ok();
            sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(id)
                .execute(&app.db)
                .await
                .ok();
        }
    });
}
