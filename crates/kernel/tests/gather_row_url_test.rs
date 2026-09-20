#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A gather row carries the address it is actually read at.
//!
//! Every shipped listing template emitted `/item/{{ row.id }}`, because the row
//! had nothing else to emit. A site built on the kernel therefore advertised a
//! UUID in every link on every listing, while the friendly URL the item itself
//! was served at sat in `url_alias` unused — the single most visible defect on
//! any public site built on it.
//!
//! A template cannot fix that on its own: resolving an alias is a database
//! lookup, and a template doing one per row is the shape this exists to avoid.
//! So the gather service resolves the whole result set at once and hands each row
//! a `url`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::HashMap;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryField, QueryFilter,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateUrlAlias, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_1A46_0000_0009;

const ITEM_TYPE: &str = "gather_row_url_test";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::List,
        items_per_page: 50,
        pager: PagerConfig {
            enabled: false,
            style: PagerStyle::Full,
            show_count: false,
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
         VALUES ($1, 'Gather Row URL Test', 'Fixture type', true, 'Title', 'core', $2) \
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
            "Gather Row URL Test",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

async fn create_item(app: &TestApp, marker: &str, label: &str) -> Uuid {
    ensure_item_type(app).await;
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
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather row url test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

async fn alias(app: &TestApp, id: Uuid, path: &str) {
    UrlAlias::create(
        &app.db,
        CreateUrlAlias {
            source: format!("/item/{id}"),
            alias: path.to_string(),
            language: Some("en".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create alias");
}

/// Run a gather over this test's items only. The marker filter is what isolates
/// one test from every other item in the shared database.
async fn gather_rows(
    app: &TestApp,
    marker: &str,
    fields: Vec<QueryField>,
) -> Vec<serde_json::Value> {
    let definition = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(ITEM_TYPE.to_string()),
        fields,
        filters: vec![QueryFilter {
            field: "title".to_string(),
            operator: FilterOperator::Contains,
            value: FilterValue::String(marker.to_string()),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
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

fn url_of(row: &serde_json::Value) -> Option<&str> {
    row.get("url").and_then(|v| v.as_str())
}

/// The defect: an aliased item was linked by UUID, because the row had no
/// address on it at all.
#[test]
fn an_aliased_row_carries_its_alias() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowurl{}", Uuid::now_v7().simple());
        let id = create_item(app, &marker, "aliased").await;
        let friendly = format!("/{marker}-friendly");
        alias(app, id, &friendly).await;

        let rows = gather_rows(app, &marker, Vec::new()).await;
        assert_eq!(rows.len(), 1, "one item matches the marker");

        assert_eq!(
            url_of(&rows[0]),
            Some(friendly.as_str()),
            "the row must carry the address the item is served at, not its UUID"
        );
        assert_ne!(
            url_of(&rows[0]),
            Some(format!("/item/{id}").as_str()),
            "a UUID link is the defect this fixes"
        );
    });
}

/// An item with no alias still gets a usable address, so a template can rely on
/// `url` unconditionally.
#[test]
fn an_unaliased_row_falls_back_to_the_item_path() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowurl{}", Uuid::now_v7().simple());
        let id = create_item(app, &marker, "bare").await;

        let rows = gather_rows(app, &marker, Vec::new()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(url_of(&rows[0]), Some(format!("/item/{id}").as_str()));
    });
}

/// A result set is resolved as a set: every row in a mixed page gets its own
/// right answer from the one lookup.
#[test]
fn a_mixed_result_set_resolves_every_row() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowurl{}", Uuid::now_v7().simple());
        let aliased = create_item(app, &marker, "one").await;
        let bare = create_item(app, &marker, "two").await;
        let friendly = format!("/{marker}-one");
        alias(app, aliased, &friendly).await;

        let rows = gather_rows(app, &marker, Vec::new()).await;
        assert_eq!(rows.len(), 2);

        let urls: Vec<&str> = rows.iter().filter_map(url_of).collect();
        assert_eq!(urls.len(), 2, "every row carries a url");
        assert!(urls.contains(&friendly.as_str()), "got {urls:?}");
        assert!(
            urls.contains(&format!("/item/{bare}").as_str()),
            "got {urls:?}"
        );
    });
}

/// A gather projecting an explicit field list that omits `id` has no item
/// address to resolve, and gets no `url` rather than a wrong one.
#[test]
fn a_row_without_an_id_gets_no_url() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowurl{}", Uuid::now_v7().simple());
        let id = create_item(app, &marker, "titleonly").await;
        alias(app, id, &format!("/{marker}-titleonly")).await;

        let rows = gather_rows(
            app,
            &marker,
            vec![QueryField {
                field_name: "title".to_string(),
                table_alias: None,
                label: None,
            }],
        )
        .await;

        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].get("id").is_none(),
            "fixture assumption: this projection omits id"
        );
        assert!(
            url_of(&rows[0]).is_none(),
            "no id means no address, and a guess would be worse than nothing"
        );
    });
}

// -------------------------------------------------------------------------
// The shipped templates
// -------------------------------------------------------------------------
//
// The service resolving a `url` is half the fix; the templates have to read it.
// These render the files that actually ship, so a template reverting to
// `/item/{{ row.id }}` fails here rather than on a live site.

/// Render a shipped template with the `format_date` filter the gather templates
/// use, which is registered by the real theme engine and is not a Tera builtin.
fn render_shipped(relative: &str, context: tera::Context) -> String {
    let path = common::project_root().join(relative);
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let mut tera = tera::Tera::default();
    tera.register_filter(
        "format_date",
        |value: &tera::Value, _: &HashMap<String, tera::Value>| Ok(value.clone()),
    );
    tera.add_raw_template("shipped", &source)
        .expect("the shipped template parses");
    tera.render("shipped", &context)
        .expect("the shipped template renders")
}

#[test]
fn the_shipped_row_template_links_to_the_resolved_url() {
    let mut context = tera::Context::new();
    context.insert(
        "row",
        &serde_json::json!({
            "id": "0193a5a0-0001-7000-8000-000000000042",
            "title": "Hello",
            "url": "/blog/hello",
        }),
    );

    let html = render_shipped("templates/gather/row.html", context);
    assert!(html.contains("href=\"/blog/hello\""), "got {html}");
    assert!(
        !html.contains("/item/0193a5a0"),
        "the UUID link is the defect this fixes: {html}"
    );
}

/// A gather whose field list omits `id` resolves to no `url`; the template must
/// still render rather than emitting `href=""`.
#[test]
fn the_shipped_row_template_falls_back_when_there_is_no_url() {
    let mut context = tera::Context::new();
    context.insert(
        "row",
        &serde_json::json!({
            "id": "0193a5a0-0001-7000-8000-000000000042",
            "title": "Hello",
        }),
    );

    let html = render_shipped("templates/gather/row.html", context);
    assert!(
        html.contains("href=\"/item/0193a5a0-0001-7000-8000-000000000042\""),
        "got {html}"
    );
}

#[test]
fn the_shipped_blog_listing_links_to_the_resolved_url() {
    let mut context = tera::Context::new();
    context.insert("query", &serde_json::json!({ "label": "Blog" }));
    context.insert(
        "rows",
        &serde_json::json!([{
            "id": "0193a5a0-0001-7000-8000-000000000042",
            "title": "Hello",
            "created": 0,
            "url": "/blog/hello",
        }]),
    );
    context.insert("pager", &serde_json::Value::Null);

    let html = render_shipped("templates/gather/query--blog_listing.html", context);
    // Both the title link and the read-more link.
    assert_eq!(
        html.matches("href=\"/blog/hello\"").count(),
        2,
        "both links on a teaser must use the resolved url: {html}"
    );
    assert!(!html.contains("/item/0193a5a0"), "got {html}");
}
