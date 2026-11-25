#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B185: a gather over one category's terms.
//!
//! `category_term` is the kernel's own built-in lightweight-record type (over
//! `category_tag`, not a plugin's table), so unlike
//! `record_gather_fixture_test.rs` this needs no WASM plugin: the registry is
//! built directly with [`RecordTypeRegistry::with_builtin`], the same call
//! `AppState::new` makes at startup.
//!
//! Requires Postgres.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use trovato_kernel::content::{
    CATEGORY_TERM_RECORD_TYPE, RecordTypeRegistry, category_term_record_type,
};
use trovato_kernel::gather::{
    CategoryService, FilterOperator, FilterValue, GatherExtensionRegistry, GatherService,
    QueryContext, QueryDefinition, QueryDisplay, QueryFilter, QuerySort, SortDirection,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateUrlAlias, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn database_url() -> String {
    trovato_test_utils::env::load_dotenv();
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string())
}

/// A standalone `GatherService` wired exactly like `AppState::new` wires one
/// for the built-in `category_term` record type: no plugin, no item service
/// (field-level access is irrelevant here — only the `category_tag` columns
/// this record type maps are ever read).
fn wire(pool: sqlx::PgPool) -> Arc<GatherService> {
    let registry = RecordTypeRegistry::default()
        .with_builtin(category_term_record_type(), &Default::default());
    assert!(registry.contains(CATEGORY_TERM_RECORD_TYPE));

    let categories =
        CategoryService::new(pool.clone(), Duration::from_secs(60), Default::default());
    let gather = GatherService::new(
        pool,
        categories,
        Arc::new(GatherExtensionRegistry::new()),
        trovato_kernel::gather::GatherConfig {
            ttl: Duration::from_secs(60),
            max_page_size: 100,
            access: trovato_kernel::gather::GatherAccessConfig::default(),
        },
        None,
        None,
        Default::default(),
    );
    gather.set_record_types(Arc::new(registry));
    gather
}

/// A fresh, short-lived vocabulary id: `category.id` is `VARCHAR(32)`, and each
/// test gets its own so parallel test functions never see one another's terms.
/// `Uuid::new_v4()` (not `now_v7()`): two ids minted microseconds apart in the
/// same test share a v7's leading timestamp bits, and this is used in full as
/// the id (`category.id` is 32 chars — exactly one hex UUID with no room for a
/// prefix).
fn fresh_category_id() -> String {
    Uuid::new_v4().simple().to_string()
}

async fn create_category(pool: &sqlx::PgPool, id: &str) {
    sqlx::query("INSERT INTO category (id, label, description, hierarchy) VALUES ($1, $1, '', 0)")
        .bind(id)
        .execute(pool)
        .await
        .expect("create test category");
}

async fn create_term(pool: &sqlx::PgPool, category_id: &str, label: &str, weight: i16) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO category_tag (id, category_id, label, description, weight, created, changed) \
         VALUES ($1, $2, $3, '', $4, 1700000000, 1700000000)",
    )
    .bind(id)
    .bind(category_id)
    .bind(label)
    .bind(weight)
    .execute(pool)
    .await
    .expect("create test term");
    id
}

async fn cleanup(pool: &sqlx::PgPool, category_ids: &[&str]) {
    for id in category_ids {
        sqlx::query("DELETE FROM category_tag WHERE category_id = $1")
            .bind(id)
            .execute(pool)
            .await
            .expect("cleanup terms");
        sqlx::query("DELETE FROM category WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .expect("cleanup category");
    }
}

fn context() -> QueryContext {
    QueryContext {
        current_user_id: None,
        viewer: Some(UserContext::anonymous()),
        url_args: HashMap::new(),
        language: None,
    }
}

/// A term gather filtered to one category, sorted weight then name — the
/// shape a site writes to list a vocabulary's terms.
fn term_gather(category_id: &str) -> QueryDefinition {
    QueryDefinition {
        record_type: Some(CATEGORY_TERM_RECORD_TYPE.to_string()),
        filters: vec![QueryFilter {
            field: "category".to_string(),
            operator: FilterOperator::Equals,
            value: FilterValue::String(category_id.to_string()),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }],
        sorts: vec![
            QuerySort {
                field: "weight".to_string(),
                direction: SortDirection::Asc,
                nulls: None,
            },
            QuerySort {
                field: "title".to_string(),
                direction: SortDirection::Asc,
                nulls: None,
            },
        ],
        ..Default::default()
    }
}

async fn pool() -> sqlx::PgPool {
    let pool = sqlx::postgres::PgPool::connect(&database_url())
        .await
        .expect("connect to test database");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("migrate the test database");
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn term_gather_orders_by_weight_then_name_and_stays_within_its_category() {
    let pool = pool().await;
    let cat_a = fresh_category_id();
    let cat_b = fresh_category_id();
    create_category(&pool, &cat_a).await;
    create_category(&pool, &cat_b).await;

    // Out of insertion order, and two terms share a weight to exercise the
    // name tie-break.
    create_term(&pool, &cat_a, "Rust", 5).await;
    create_term(&pool, &cat_a, "Zebra", 1).await;
    create_term(&pool, &cat_a, "Apple", 1).await;
    // A term in another vocabulary: must never appear in cat_a's listing.
    create_term(&pool, &cat_b, "Intruder", 0).await;

    let gather = wire(pool.clone());
    let result = gather
        .execute_definition_with_stages(
            &term_gather(&cat_a),
            &QueryDisplay::default(),
            1,
            HashMap::new(),
            &[LIVE_STAGE_ID],
            &context(),
        )
        .await
        .expect("gather execution");

    assert_eq!(result.total, 3, "only cat_a's three terms: {result:?}");
    let labels: Vec<&str> = result
        .items
        .iter()
        .map(|r| r.get("title").and_then(|v| v.as_str()).expect("title"))
        .collect();
    assert_eq!(
        labels,
        vec!["Apple", "Zebra", "Rust"],
        "weight asc, then name asc within a tied weight: {labels:?}"
    );
    assert!(
        result
            .items
            .iter()
            .all(|r| r.get("title").and_then(|v| v.as_str()) != Some("Intruder")),
        "never returns another category's terms: {:?}",
        result.items
    );
    for row in &result.items {
        assert!(row.get("weight").is_some(), "carries weight: {row}");
        assert!(row.get("id").is_some(), "carries id: {row}");
        assert!(row.get("url").is_some(), "carries url: {row}");
    }

    cleanup(&pool, &[&cat_a, &cat_b]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn term_gather_pages() {
    let pool = pool().await;
    let cat = fresh_category_id();
    create_category(&pool, &cat).await;
    create_term(&pool, &cat, "A", 0).await;
    create_term(&pool, &cat, "B", 1).await;
    create_term(&pool, &cat, "C", 2).await;

    let gather = wire(pool.clone());
    let display = QueryDisplay {
        items_per_page: Some(2),
        ..Default::default()
    };

    let page1 = gather
        .execute_definition_with_stages(
            &term_gather(&cat),
            &display,
            1,
            HashMap::new(),
            &[LIVE_STAGE_ID],
            &context(),
        )
        .await
        .expect("page 1");
    assert_eq!(page1.total, 3);
    assert_eq!(page1.total_pages, 2);
    assert!(page1.has_next);
    assert!(!page1.has_prev);
    let page1_labels: Vec<&str> = page1
        .items
        .iter()
        .map(|r| r.get("title").and_then(|v| v.as_str()).unwrap())
        .collect();
    assert_eq!(page1_labels, vec!["A", "B"]);

    let page2 = gather
        .execute_definition_with_stages(
            &term_gather(&cat),
            &display,
            2,
            HashMap::new(),
            &[LIVE_STAGE_ID],
            &context(),
        )
        .await
        .expect("page 2");
    assert!(!page2.has_next);
    assert!(page2.has_prev);
    let page2_labels: Vec<&str> = page2
        .items
        .iter()
        .map(|r| r.get("title").and_then(|v| v.as_str()).unwrap())
        .collect();
    assert_eq!(page2_labels, vec!["C"]);

    cleanup(&pool, &[&cat]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn term_gather_carries_alias_when_set_else_term_path() {
    let pool = pool().await;
    let cat = fresh_category_id();
    create_category(&pool, &cat).await;
    let aliased = create_term(&pool, &cat, "Aliased", 0).await;
    let plain = create_term(&pool, &cat, "Plain", 1).await;

    UrlAlias::create(
        &pool,
        CreateUrlAlias {
            source: format!("/term/{aliased}"),
            alias: format!("/nice-term-{}", &aliased.simple().to_string()[..8]),
            language: None,
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create alias");

    let gather = wire(pool.clone());
    let result = gather
        .execute_definition_with_stages(
            &term_gather(&cat),
            &QueryDisplay::default(),
            1,
            HashMap::new(),
            &[LIVE_STAGE_ID],
            &context(),
        )
        .await
        .expect("gather execution");

    let row = |id: Uuid| {
        result
            .items
            .iter()
            .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(id.to_string()).as_deref())
            .unwrap_or_else(|| panic!("row {id} present"))
    };
    let aliased_url = row(aliased).get("url").and_then(|v| v.as_str()).unwrap();
    assert!(
        aliased_url.starts_with("/nice-term-"),
        "carries its alias: {aliased_url}"
    );
    let plain_url = row(plain).get("url").and_then(|v| v.as_str()).unwrap();
    assert_eq!(
        plain_url,
        format!("/term/{plain}"),
        "falls back to /term/{{id}} with no alias set"
    );

    cleanup(&pool, &[&cat]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn term_gather_without_a_bound_category_filter_is_refused() {
    let pool = pool().await;
    let cat = fresh_category_id();
    create_category(&pool, &cat).await;
    create_term(&pool, &cat, "Lonely", 0).await;

    let gather = wire(pool.clone());
    let unfiltered = QueryDefinition {
        record_type: Some(CATEGORY_TERM_RECORD_TYPE.to_string()),
        ..Default::default()
    };
    let err = gather
        .execute_definition_with_stages(
            &unfiltered,
            &QueryDisplay::default(),
            1,
            HashMap::new(),
            &[LIVE_STAGE_ID],
            &context(),
        )
        .await
        .expect_err("a category-term gather with no category filter must be refused");
    assert!(
        format!("{err:#}").contains("category"),
        "error names the missing category filter: {err:#}"
    );

    // Also refused when the filter is exposed and left unanswered: it is
    // dropped before resolution, leaving the same unfiltered state.
    let exposed_unanswered = QueryDefinition {
        record_type: Some(CATEGORY_TERM_RECORD_TYPE.to_string()),
        filters: vec![QueryFilter {
            field: "category".to_string(),
            operator: FilterOperator::Equals,
            value: FilterValue::Null(()),
            exposed: true,
            exposed_label: None,
            widget: Default::default(),
        }],
        ..Default::default()
    };
    gather
        .execute_definition_with_stages(
            &exposed_unanswered,
            &QueryDisplay::default(),
            1,
            HashMap::new(),
            &[LIVE_STAGE_ID],
            &context(),
        )
        .await
        .expect_err("an exposed, unanswered category filter must be refused too");

    cleanup(&pool, &[&cat]).await;
}

#[test]
fn saving_a_term_gather_with_no_category_filter_is_rejected() {
    let unfiltered = QueryDefinition {
        record_type: Some(CATEGORY_TERM_RECORD_TYPE.to_string()),
        ..Default::default()
    };
    let errors = GatherService::validate_definition(&unfiltered);
    assert!(
        errors.iter().any(|e| e.contains("category")),
        "save-time validation flags the missing filter: {errors:?}"
    );

    let filtered = term_gather("whatever");
    let errors = GatherService::validate_definition(&filtered);
    assert!(
        errors.is_empty(),
        "a gather with a bound category filter is valid: {errors:?}"
    );
}
