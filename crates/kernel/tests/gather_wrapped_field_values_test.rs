#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B152: gather reads a `fields.<name>` value the way `scalar_of` in
//! `crates/kernel/src/content/field_value.rs` does — a stored `{"value": …}`
//! wrapper unwraps the same as a bare scalar — through every reader: filter
//! comparison (`Equals`/`NotEquals`/`In`/`Contains`), sort, the exposed-filter
//! and `UrlArg` contextual paths, and the two hand-written option-list queries
//! in `GatherService` (`fetch_distinct_values`, `fetch_faceted_distinct_values`).
//!
//! Also B120: a filter value spelled as an unhyphenated or uppercase UUID
//! deserializes as `FilterValue::String` holding that exact text (not
//! `FilterValue::Uuid`, which would silently rewrite it to the canonical
//! lowercase-hyphenated spelling before comparing).
//!
//! Requires Postgres (the shared `TestApp`); runs in CI.

mod common;

use common::{TestApp, run_test, shared_app};
use std::collections::HashMap;
use trovato_kernel::gather::types::ContextualValue;
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter, QuerySort, SortDirection,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// This file's own content type, so its rows never meet another file's.
const ITEM_TYPE: &str = "b152_wrapped_fixture";

// Deterministic ids: seeding is `ON CONFLICT ... DO UPDATE`, so re-running the
// suite against a persistent database never grows the fixture or collides
// with another file's rows.
const BARGA_BARE: &str = "0193b152-0001-7000-8000-000000000001";
const BARGA_WRAPPED: &str = "0193b152-0001-7000-8000-000000000002";
const BARGA_WRAPPED_WITH_FORMAT: &str = "0193b152-0001-7000-8000-000000000003";
const LUCCA_BARE: &str = "0193b152-0001-7000-8000-000000000004";
const LUCCA_WRAPPED: &str = "0193b152-0001-7000-8000-000000000005";
const WEIGHT_BARE_7: &str = "0193b152-0001-7000-8000-000000000006";
const WEIGHT_WRAPPED_12: &str = "0193b152-0001-7000-8000-000000000007";
const WEIGHT_WRAPPED_3: &str = "0193b152-0001-7000-8000-000000000008";
const UUID_TEXT_UNHYPHENATED: &str = "0193b152-0001-7000-8000-000000000009";
const UUID_TEXT_UPPERCASE: &str = "0193b152-0001-7000-8000-00000000000a";
const UUID_TEXT_CANONICAL: &str = "0193b152-0001-7000-8000-00000000000b";

/// The uuid whose text spellings `field_external_id` stores, picked for hex
/// letters so its uppercase spelling actually differs from its canonical one.
fn sample_uuid() -> Uuid {
    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("valid uuid literal")
}

fn admin_ctx() -> QueryContext {
    QueryContext {
        viewer: Some(UserContext::administrator(
            Uuid::nil(),
            vec!["administer site".to_string()],
        )),
        ..QueryContext::default()
    }
}

fn test_display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::Table,
        items_per_page: Some(50),
        pager: PagerConfig {
            enabled: true,
            style: PagerStyle::Full,
            show_count: true,
            zero_based: false,
        },
        empty_text: None,
        header: None,
        footer: None,
        canonical_url: None,
        routes: Vec::new(),
        feed: None,
    }
}

fn unset_filter(field: &str, operator: FilterOperator) -> QueryFilter {
    QueryFilter {
        field: field.to_string(),
        operator,
        value: FilterValue::Null(()),
        exposed: true,
        exposed_label: None,
        widget: Default::default(),
    }
}

fn filter(field: &str, operator: FilterOperator, value: FilterValue) -> QueryFilter {
    QueryFilter {
        field: field.to_string(),
        operator,
        value,
        exposed: false,
        exposed_label: None,
        widget: Default::default(),
    }
}

/// Seeds the fixture: 3 `field_city` "Barga" rows (bare, wrapped, wrapped
/// with a sibling `format` key), 2 `field_city` "Lucca" rows (bare, wrapped —
/// the 5 carry `field_country` too, "Italy" bare on the Barga rows and
/// wrapped on the Lucca rows, so the facet test exercises the same reader on
/// a *scope* condition), 3 `field_weight` rows with none of `field_city` (bare
/// `7`, wrapped `12`, wrapped `3`), and 3 `field_external_id` rows holding the
/// unhyphenated, uppercase and canonical text spellings of one uuid.
/// Idempotent — safe to call from any test.
async fn seed(app: &TestApp) {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, 'B152 wrapped-field-value fixture', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .execute(&app.db)
    .await
    .expect("seed item_type");

    let now = chrono::Utc::now().timestamp();
    let nil_author = Uuid::nil();
    let uuid = sample_uuid();

    let rows: Vec<(&str, &str, serde_json::Value)> = vec![
        (
            BARGA_BARE,
            "Barga bare",
            serde_json::json!({"field_city": "Barga", "field_country": "Italy"}),
        ),
        (
            BARGA_WRAPPED,
            "Barga wrapped",
            serde_json::json!({
                "field_city": {"value": "Barga"},
                "field_country": "Italy",
            }),
        ),
        (
            BARGA_WRAPPED_WITH_FORMAT,
            "Barga wrapped with format",
            serde_json::json!({
                "field_city": {"value": "Barga", "format": "plain_text"},
                "field_country": "Italy",
            }),
        ),
        (
            LUCCA_BARE,
            "Lucca bare",
            serde_json::json!({
                "field_city": "Lucca",
                "field_country": {"value": "Italy"},
            }),
        ),
        (
            LUCCA_WRAPPED,
            "Lucca wrapped",
            serde_json::json!({
                "field_city": {"value": "Lucca"},
                "field_country": {"value": "Italy"},
            }),
        ),
        (
            WEIGHT_BARE_7,
            "Weight bare 7",
            serde_json::json!({"field_weight": 7}),
        ),
        (
            WEIGHT_WRAPPED_12,
            "Weight wrapped 12",
            serde_json::json!({"field_weight": {"value": 12}}),
        ),
        (
            WEIGHT_WRAPPED_3,
            "Weight wrapped 3",
            serde_json::json!({"field_weight": {"value": 3}}),
        ),
        (
            UUID_TEXT_UNHYPHENATED,
            "Uuid text unhyphenated",
            serde_json::json!({"field_external_id": uuid.simple().to_string()}),
        ),
        (
            UUID_TEXT_UPPERCASE,
            "Uuid text uppercase",
            serde_json::json!({"field_external_id": uuid.to_string().to_uppercase()}),
        ),
        (
            UUID_TEXT_CANONICAL,
            "Uuid text canonical",
            serde_json::json!({"field_external_id": uuid.to_string()}),
        ),
    ];

    for (id, title, fields) in rows {
        let id: Uuid = id.parse().expect("valid fixture id literal");
        sqlx::query(
            "INSERT INTO item \
                (id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id) \
             VALUES ($1, $2, $3, $4, 1, $5, $5, 0, 0, $6, $7) \
             ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title, fields = EXCLUDED.fields",
        )
        .bind(id)
        .bind(ITEM_TYPE)
        .bind(title)
        .bind(nil_author)
        .bind(now)
        .bind(&fields)
        .bind(LIVE_STAGE_ID)
        .execute(&app.db)
        .await
        .expect("seed fixture item");
    }
}

fn city_definition(op: FilterOperator, value: FilterValue) -> QueryDefinition {
    QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(ITEM_TYPE.to_string()),
        filters: vec![filter("fields.field_city", op, value)],
        ..Default::default()
    }
}

// =============================================================================
// Filter comparisons read the wrapper the same as the bare value
// =============================================================================

#[test]
fn equals_matches_bare_and_wrapped_barga_rows() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let definition = city_definition(
            FilterOperator::Equals,
            FilterValue::String("Barga".to_string()),
        );
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("equals barga");

        assert_eq!(result.total, 3, "{:?}", result.items);
    });
}

#[test]
fn in_matches_every_bare_and_wrapped_row_of_either_city() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let definition = city_definition(
            FilterOperator::In,
            FilterValue::List(vec![
                FilterValue::String("Barga".to_string()),
                FilterValue::String("Lucca".to_string()),
            ]),
        );
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("in barga/lucca");

        assert_eq!(result.total, 5, "{:?}", result.items);
    });
}

#[test]
fn not_equals_barga_returns_only_the_lucca_rows() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let definition = city_definition(
            FilterOperator::NotEquals,
            FilterValue::String("Barga".to_string()),
        );
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("not equals barga");

        // Exactly the 2 Lucca rows, not every non-Barga fixture row: the
        // weight/uuid rows carry no `field_city` at all, and `NULL <> 'Barga'`
        // is NULL, not TRUE, so they stay excluded too.
        assert_eq!(result.total, 2, "{:?}", result.items);
    });
}

#[test]
fn contains_arg_matches_only_the_barga_rows() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let definition = city_definition(
            FilterOperator::Contains,
            FilterValue::String("arg".to_string()),
        );
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("contains arg");

        assert_eq!(result.total, 3, "{:?}", result.items);
    });
}

// =============================================================================
// Exposed filters and `UrlArg` contextual filters go through the same reader
// =============================================================================

#[test]
fn an_exposed_equals_filter_matches_the_same_three_rows() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some(ITEM_TYPE.to_string()),
            filters: vec![unset_filter("fields.field_city", FilterOperator::Equals)],
            ..Default::default()
        };
        let mut exposed = HashMap::new();
        exposed.insert(
            "fields.field_city".to_string(),
            FilterValue::String("Barga".to_string()),
        );

        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                exposed,
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("exposed equals barga");

        assert_eq!(result.total, 3, "{:?}", result.items);
    });
}

#[test]
fn a_url_arg_contextual_filter_matches_the_same_three_rows() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some(ITEM_TYPE.to_string()),
            filters: vec![filter(
                "fields.field_city",
                FilterOperator::Equals,
                FilterValue::Contextual(ContextualValue::UrlArg("city".to_string())),
            )],
            ..Default::default()
        };
        let mut url_args = HashMap::new();
        url_args.insert("city".to_string(), "Barga".to_string());
        let context = QueryContext {
            url_args,
            ..admin_ctx()
        };

        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &context,
            )
            .await
            .expect("url_arg equals barga");

        assert_eq!(result.total, 3, "{:?}", result.items);
    });
}

// =============================================================================
// Option lists (`fetch_distinct_values`, `fetch_faceted_distinct_values`) read
// text, never the wrapper, and the facet's own scope condition unwraps too
// =============================================================================

#[test]
fn distinct_values_option_list_is_unwrapped_text() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let values = app
            .state
            .gather()
            .fetch_distinct_values("fields.field_city", ITEM_TYPE)
            .await
            .expect("distinct values");

        assert_eq!(values, vec!["Barga".to_string(), "Lucca".to_string()]);
    });
}

/// The Lucca rows store `field_country` wrapped; before B152 the facet scope
/// condition read it as literal JSON text, never matched `"Italy"`, and
/// dropped both Lucca rows from the option list — leaving only `["Barga"]`.
#[test]
fn faceted_distinct_values_unwraps_both_the_target_and_the_scope_field() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let all_exposed = vec![unset_filter("fields.field_country", FilterOperator::Equals)];
        let mut active = HashMap::new();
        active.insert(
            "fields.field_country".to_string(),
            FilterValue::String("Italy".to_string()),
        );

        let values = app
            .state
            .gather()
            .fetch_faceted_distinct_values(
                "fields.field_city",
                ITEM_TYPE,
                &all_exposed,
                "fields.field_city",
                &active,
            )
            .await
            .expect("faceted distinct values");

        assert_eq!(values, vec!["Barga".to_string(), "Lucca".to_string()]);
    });
}

// =============================================================================
// Sorting reads the wrapper's payload as jsonb, so a wrapped number still
// sorts numerically alongside a bare one
// =============================================================================

#[test]
fn sort_on_a_wrapped_integer_field_orders_numerically() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;

        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some(ITEM_TYPE.to_string()),
            filters: vec![filter(
                "fields.field_weight",
                FilterOperator::IsNotNull,
                FilterValue::Null(()),
            )],
            sorts: vec![QuerySort {
                field: "fields.field_weight".to_string(),
                direction: SortDirection::Asc,
                nulls: None,
            }],
            ..Default::default()
        };

        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("sort wrapped weight");

        let titles: Vec<&str> = result
            .items
            .iter()
            .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
            .collect();
        assert_eq!(
            titles,
            vec!["Weight wrapped 3", "Weight bare 7", "Weight wrapped 12"],
            "{:?}",
            result.items
        );
    });
}

// =============================================================================
// B120: an unhyphenated or uppercase UUID spelling compares as the exact text
// it was written as, not the canonical spelling the untagged `Uuid` variant
// used to coerce it to
// =============================================================================

fn uuid_text_definition(filter_value_json: &str) -> QueryDefinition {
    let json = serde_json::json!({
        "base_table": "item",
        "item_type": ITEM_TYPE,
        "filters": [{
            "field": "fields.field_external_id",
            "operator": "equals",
            "value": filter_value_json,
        }],
    });
    serde_json::from_value(json).expect("definition parses")
}

#[test]
fn unhyphenated_uuid_filter_value_deserializes_as_string_and_matches_its_exact_text() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;
        let unhyphenated = sample_uuid().simple().to_string();

        let definition = uuid_text_definition(&unhyphenated);
        match &definition.filters[0].value {
            FilterValue::String(s) => assert_eq!(s, &unhyphenated),
            other => panic!("expected FilterValue::String, got {other:?}"),
        }

        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("equals unhyphenated uuid text");

        assert_eq!(result.total, 1, "{:?}", result.items);
        assert_eq!(
            result.items[0]["title"].as_str(),
            Some("Uuid text unhyphenated")
        );
    });
}

#[test]
fn uppercase_uuid_filter_value_deserializes_as_string_and_matches_its_exact_text() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;
        let uppercase = sample_uuid().to_string().to_uppercase();

        let definition = uuid_text_definition(&uppercase);
        match &definition.filters[0].value {
            FilterValue::String(s) => assert_eq!(s, &uppercase),
            other => panic!("expected FilterValue::String, got {other:?}"),
        }

        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("equals uppercase uuid text");

        assert_eq!(result.total, 1, "{:?}", result.items);
        assert_eq!(
            result.items[0]["title"].as_str(),
            Some("Uuid text uppercase")
        );
    });
}

#[test]
fn canonical_uuid_filter_value_still_deserializes_as_uuid_and_matches_its_exact_text() {
    run_test(async {
        let app = shared_app().await;
        seed(app).await;
        let canonical = sample_uuid().to_string();

        let definition = uuid_text_definition(&canonical);
        match &definition.filters[0].value {
            FilterValue::Uuid(u) => assert_eq!(*u, sample_uuid()),
            other => panic!("expected FilterValue::Uuid, got {other:?}"),
        }

        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("equals canonical uuid text");

        assert_eq!(result.total, 1, "{:?}", result.items);
        assert_eq!(
            result.items[0]["title"].as_str(),
            Some("Uuid text canonical")
        );
    });
}
