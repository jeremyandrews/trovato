#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Sorting a gather by a `fields.*` path compares values of their own type.
//!
//! `ORDER BY` used to extract with `->>`, which hands PostgreSQL text whatever
//! the JSON type is, so a numeric field sorted ascending came back 0, 10, 100,
//! 110, ... 20, 200: a documentation index sorted by `fields.weight` put Part 7
//! at the top and Parts 1 to 6 at the bottom. Extracting with `->` gives typed
//! comparison instead, which is right for numbers and strings both.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::HashMap;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, NullsOrder, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter, QuerySort, SortDirection,
};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_1A46_0000_0003;

const ITEM_TYPE: &str = "gather_sort_test";

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
}

fn display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::Table,
        items_per_page: 50,
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

async fn ensure_item_type(app: &TestApp) {
    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Gather Sort Test', 'Fixture type for gather sort tests', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed item type");
    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .create(
            ITEM_TYPE,
            "Gather Sort Test",
            Some("Fixture type for gather sort tests"),
            settings,
        )
        .await
        .ok();
}

/// An item whose title carries `marker`, with the given fields.
async fn create_item(app: &TestApp, marker: &str, label: &str, fields: serde_json::Value) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: format!("{marker} {label}"),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather sort test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// Run a gather over this test's items only, sorted by `sort_field`.
///
/// The marker filter is what isolates one test from every other item in the
/// shared database, which is the only reason the ordering assertions can be
/// exact.
async fn sorted_titles(
    app: &TestApp,
    marker: &str,
    sort_field: &str,
    direction: SortDirection,
    nulls: Option<NullsOrder>,
) -> Vec<String> {
    let definition = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(ITEM_TYPE.to_string()),
        filters: vec![QueryFilter {
            field: "title".to_string(),
            operator: FilterOperator::Contains,
            value: FilterValue::String(marker.to_string()),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }],
        sorts: vec![QuerySort {
            field: sort_field.to_string(),
            direction,
            nulls,
        }],
        stage_aware: true,
        ..Default::default()
    };

    let ctx = QueryContext {
        current_user_id: None,
        viewer: Some(admin()),
        url_args: HashMap::new(),
        language: None,
    };

    let result = app
        .state
        .gather()
        .execute_definition(
            &definition,
            &display(),
            1,
            HashMap::new(),
            LIVE_STAGE_ID,
            &ctx,
        )
        .await
        .expect("gather executes");

    result
        .items
        .iter()
        .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
        .map(|t| t.rsplit(' ').next().unwrap_or(t).to_string())
        .collect()
}

async fn cleanup(app: &TestApp, ids: &[Uuid]) {
    for id in ids {
        app.state.items().delete(*id, &admin()).await.ok();
    }
}

/// The reported bug, in one assertion: numbers sort as numbers.
///
/// 3 before 20 before 100. Under text extraction this came back 100, 20, 3.
#[test]
fn a_numeric_field_sorts_numerically() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let marker = format!("numsort{}", Uuid::now_v7().simple());

        let mut ids = Vec::new();
        for weight in [20, 100, 3] {
            ids.push(
                create_item(
                    app,
                    &marker,
                    &format!("w{weight}"),
                    serde_json::json!({ "weight": weight }),
                )
                .await,
            );
        }

        let order = sorted_titles(app, &marker, "fields.weight", SortDirection::Asc, None).await;
        assert_eq!(
            order,
            vec!["w3", "w20", "w100"],
            "ascending by a numeric field must be 3, 20, 100"
        );

        let order = sorted_titles(app, &marker, "fields.weight", SortDirection::Desc, None).await;
        assert_eq!(
            order,
            vec!["w100", "w20", "w3"],
            "descending is the reverse"
        );

        cleanup(app, &ids).await;
    });
}

/// A string field still sorts lexically. jsonb comparison is typed, so one
/// operator serves both and nothing had to be declared.
#[test]
fn a_string_field_still_sorts_lexically() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let marker = format!("strsort{}", Uuid::now_v7().simple());

        let mut ids = Vec::new();
        for city in ["milano", "amsterdam", "zagreb"] {
            ids.push(create_item(app, &marker, city, serde_json::json!({ "city": city })).await);
        }

        let order = sorted_titles(app, &marker, "fields.city", SortDirection::Asc, None).await;
        assert_eq!(order, vec!["amsterdam", "milano", "zagreb"]);

        cleanup(app, &ids).await;
    });
}

/// A nested path is extracted the same way all the way down, so a number under
/// two keys sorts as a number too.
#[test]
fn a_nested_numeric_path_sorts_numerically() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let marker = format!("nestsort{}", Uuid::now_v7().simple());

        let mut ids = Vec::new();
        for order in [20, 100, 3] {
            ids.push(
                create_item(
                    app,
                    &marker,
                    &format!("n{order}"),
                    serde_json::json!({ "meta": { "order": order } }),
                )
                .await,
            );
        }

        let sorted =
            sorted_titles(app, &marker, "fields.meta.order", SortDirection::Asc, None).await;
        assert_eq!(sorted, vec!["n3", "n20", "n100"]);

        cleanup(app, &ids).await;
    });
}

/// The `NULLS FIRST` / `NULLS LAST` branch still decides where a missing value
/// goes, on either side of the change.
#[test]
fn nulls_ordering_is_unaffected() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let marker = format!("nullsort{}", Uuid::now_v7().simple());

        let mut ids = vec![
            create_item(app, &marker, "w5", serde_json::json!({ "weight": 5 })).await,
            create_item(app, &marker, "w40", serde_json::json!({ "weight": 40 })).await,
        ];
        // No `weight` key at all: the JSONB extraction is SQL NULL.
        ids.push(create_item(app, &marker, "none", serde_json::json!({ "other": 1 })).await);

        let first = sorted_titles(
            app,
            &marker,
            "fields.weight",
            SortDirection::Asc,
            Some(NullsOrder::First),
        )
        .await;
        assert_eq!(first, vec!["none", "w5", "w40"]);

        let last = sorted_titles(
            app,
            &marker,
            "fields.weight",
            SortDirection::Asc,
            Some(NullsOrder::Last),
        )
        .await;
        assert_eq!(last, vec!["w5", "w40", "none"]);

        cleanup(app, &ids).await;
    });
}
