#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A gather tile can name the item its page shows (B186).
//!
//! `ContextualValue::CurrentItem`/`CurrentItemField` resolve from
//! `QueryContext::current_item_id`/`current_item_fields`, which the item page
//! sets from the item it renders. One tile placed once, filtering on
//! `current_item`, so serves every item page: "the other articles" (exclude
//! the one shown), "related recipes" (share a field of the one shown). On a
//! page with no current item, such a filter must make the gather return
//! nothing — never an unfiltered list.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::HashMap;

use common::{TestApp, defer_cleanup, run_test, shared_app};
use trovato_kernel::gather::{
    ContextualValue, DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle,
    QueryContext, QueryDefinition, QueryDisplay, QueryFilter,
};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_B186_0000_0002;

const ITEM_TYPE: &str = "gather_current_item_test";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::List,
        items_per_page: Some(50),
        pager: PagerConfig {
            enabled: false,
            style: PagerStyle::Full,
            show_count: false,
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
         VALUES ($1, 'Gather Current Item Test', 'Fixture type', true, 'Title', 'core', $2) \
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
        .replace(
            ITEM_TYPE,
            "Gather Current Item Test",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

/// Create a fixture item, with arbitrary `fields` (used for the tag tests).
async fn create_item(
    app: &TestApp,
    marker: &str,
    label: &str,
    fields: serde_json::Value,
) -> (Uuid, serde_json::Value) {
    ensure_item_type(app).await;
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: format!("{marker} {label}"),
                author_id: trovato_kernel::models::user::ANONYMOUS_USER_ID,
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather current item test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item");
    let (id, pool) = (item.id, app.db.clone());
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    (item.id, item.fields)
}

/// A gather over this test's items only (the marker filter isolates one test
/// from every other item in the shared database), with `extra_filters` ANDed
/// in and a resolved `QueryContext` naming `current_item`, if any.
async fn gather_rows(
    app: &TestApp,
    marker: &str,
    extra_filters: Vec<QueryFilter>,
    current_item: Option<(Uuid, serde_json::Value)>,
) -> Vec<serde_json::Value> {
    let mut filters = vec![QueryFilter {
        field: "title".to_string(),
        operator: FilterOperator::Contains,
        value: FilterValue::String(marker.to_string()),
        exposed: false,
        exposed_label: None,
        widget: Default::default(),
    }];
    filters.extend(extra_filters);

    let definition = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(ITEM_TYPE.to_string()),
        filters,
        stage_aware: true,
        ..Default::default()
    };

    let ctx = QueryContext {
        current_user_id: None,
        viewer: Some(admin()),
        url_args: HashMap::new(),
        language: None,
        current_item_id: current_item.as_ref().map(|(id, _)| *id),
        current_item_fields: current_item.map(|(_, fields)| fields),
    };

    app.state
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
        .expect("gather executes")
        .items
}

fn titles(rows: &[serde_json::Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| r.get("title").and_then(|t| t.as_str()).map(String::from))
        .collect()
}

/// `not_equals current_item` excludes exactly the item the page shows: "the
/// other articles" on article A lists B and C, not A; on article B lists A
/// and C, not B.
#[test]
fn not_equals_current_item_excludes_only_the_item_shown() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("othersart{}", Uuid::now_v7().simple());

        let a = create_item(app, &marker, "A", serde_json::json!({})).await;
        let b = create_item(app, &marker, "B", serde_json::json!({})).await;
        let c = create_item(app, &marker, "C", serde_json::json!({})).await;

        let not_equals_current_item = vec![QueryFilter {
            field: "id".to_string(),
            operator: FilterOperator::NotEquals,
            value: FilterValue::Contextual(ContextualValue::CurrentItem),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }];

        let on_a = gather_rows(
            app,
            &marker,
            not_equals_current_item.clone(),
            Some(a.clone()),
        )
        .await;
        let on_a_titles = titles(&on_a);
        assert_eq!(on_a.len(), 2, "got {on_a_titles:?}");
        assert!(
            !on_a_titles.contains(&format!("{marker} A")),
            "{on_a_titles:?}"
        );
        assert!(
            on_a_titles.contains(&format!("{marker} B")),
            "{on_a_titles:?}"
        );
        assert!(
            on_a_titles.contains(&format!("{marker} C")),
            "{on_a_titles:?}"
        );

        let on_b = gather_rows(app, &marker, not_equals_current_item, Some(b.clone())).await;
        let on_b_titles = titles(&on_b);
        assert_eq!(on_b.len(), 2, "got {on_b_titles:?}");
        assert!(
            !on_b_titles.contains(&format!("{marker} B")),
            "{on_b_titles:?}"
        );
        assert!(
            on_b_titles.contains(&format!("{marker} A")),
            "{on_b_titles:?}"
        );
        assert!(
            on_b_titles.contains(&format!("{marker} C")),
            "{on_b_titles:?}"
        );

        // Unused but keeps `c` from looking dead to a reader scanning the test.
        let _ = &c;
    });
}

/// `current_item.fields.<name>`, with `has_any_tag`, lists items sharing a
/// tag with the item shown — excluding it when combined with `not_equals`.
#[test]
fn current_item_field_has_any_tag_finds_items_sharing_a_tag() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("relatedrec{}", Uuid::now_v7().simple());
        let tag_shared = Uuid::now_v7().to_string();
        let tag_unique_to_r1 = Uuid::now_v7().to_string();
        let tag_unique_to_r3 = Uuid::now_v7().to_string();

        let r1 = create_item(
            app,
            &marker,
            "R1",
            serde_json::json!({ "tags": [tag_shared, tag_unique_to_r1] }),
        )
        .await;
        let r2 = create_item(
            app,
            &marker,
            "R2",
            serde_json::json!({ "tags": [tag_shared] }),
        )
        .await;
        let _r3 = create_item(
            app,
            &marker,
            "R3",
            serde_json::json!({ "tags": [tag_unique_to_r3] }),
        )
        .await;

        let related_by_tag_excluding_self = vec![
            QueryFilter {
                field: "id".to_string(),
                operator: FilterOperator::NotEquals,
                value: FilterValue::Contextual(ContextualValue::CurrentItem),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            },
            QueryFilter {
                field: "fields.tags".to_string(),
                operator: FilterOperator::HasAnyTag,
                value: FilterValue::Contextual(ContextualValue::CurrentItemField(
                    "tags".to_string(),
                )),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            },
        ];

        let on_r1 = gather_rows(
            app,
            &marker,
            related_by_tag_excluding_self,
            Some(r1.clone()),
        )
        .await;
        let on_r1_titles = titles(&on_r1);
        assert_eq!(on_r1.len(), 1, "got {on_r1_titles:?}");
        assert_eq!(on_r1_titles[0], format!("{marker} R2"));

        let _ = &r2;
    });
}

/// A page with no current item makes a filter naming `current_item` render
/// nothing — never an unfiltered list of every matching item.
#[test]
fn missing_current_item_renders_nothing() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("noitemctx{}", Uuid::now_v7().simple());

        create_item(app, &marker, "A", serde_json::json!({})).await;
        create_item(app, &marker, "B", serde_json::json!({})).await;

        let not_equals_current_item = vec![QueryFilter {
            field: "id".to_string(),
            operator: FilterOperator::NotEquals,
            value: FilterValue::Contextual(ContextualValue::CurrentItem),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }];

        let rows = gather_rows(app, &marker, not_equals_current_item, None).await;
        assert!(
            rows.is_empty(),
            "a gather naming current_item with no current item must render nothing, got {rows:?}"
        );
    });
}
