#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A gather row carries its author's display name (B166).
//!
//! The item page resolves `author.name` (and `author.url`, viewer-permitted)
//! through `UserService::author_views`; a gather listing row gets the same,
//! so a theme built on a gather prints "by {{ row.author.name }}" without a
//! per-row query. `SELECT item.*` rows (the common case: a gather with no
//! explicit field list) always carry `author_id`, exactly as they always
//! carried `id` for `resolve_row_urls` (`gather_row_url_test.rs`) — the whole
//! result set is resolved in one query, never one per row, through the same
//! cached lookup the item page and `/api/item` use.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::{TestApp, run_test, shared_app};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryField, QueryFilter,
};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::user::ANONYMOUS_USER_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_1B66_0000_0002;

const ITEM_TYPE: &str = "gather_row_author_test";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A signed-in viewer holding no permissions beyond the authenticated-user
/// default: sees the name, never a link.
fn plain_viewer() -> UserContext {
    UserContext::authenticated(Uuid::now_v7(), Vec::new())
}

/// A signed-in viewer who may see a profile.
fn profile_viewer() -> UserContext {
    UserContext::authenticated(Uuid::now_v7(), vec!["access user profiles".to_string()])
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
         VALUES ($1, 'Gather Row Author Test', 'Fixture type', true, 'Title', 'core', $2) \
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
            "Gather Row Author Test",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

async fn create_item(app: &TestApp, marker: &str, label: &str, author_id: Uuid) -> Uuid {
    ensure_item_type(app).await;
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: format!("{marker} {label}"),
                author_id,
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather row author test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// Run a gather over this test's items only, `fields` empty by default
/// (`SELECT item.*`, carrying `author_id`). The marker filter is what
/// isolates one test from every other item in the shared database.
async fn gather_rows(
    app: &TestApp,
    marker: &str,
    fields: Vec<QueryField>,
    viewer: UserContext,
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
        viewer: Some(viewer),
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

fn author_name(row: &serde_json::Value) -> Option<&str> {
    row.get("author")?.get("name")?.as_str()
}

fn author_url(row: &serde_json::Value) -> Option<&str> {
    row.get("author")?.get("url")?.as_str()
}

/// The defect: a listing row had no author at all, so a theme had to
/// hardcode a name or print nothing.
#[test]
fn a_row_carries_its_authors_name() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowauthor{}", Uuid::now_v7().simple());
        let name = format!("b166gatherauthor-{}", Uuid::now_v7().simple());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let author_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .expect("author id");

        create_item(app, &marker, "one", author_id).await;

        let rows = gather_rows(app, &marker, Vec::new(), plain_viewer()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(author_name(&rows[0]), Some(name.as_str()), "got {rows:?}");
        assert_eq!(
            author_url(&rows[0]),
            None,
            "no permission, no link: {rows:?}"
        );
    });
}

/// `access user profiles`: the row's author gets a link too.
#[test]
fn a_row_links_the_author_when_the_viewer_may_see_profiles() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowauthor{}", Uuid::now_v7().simple());
        let name = format!("b166gatherlinked-{}", Uuid::now_v7().simple());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let author_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .expect("author id");

        create_item(app, &marker, "linked", author_id).await;

        let rows = gather_rows(app, &marker, Vec::new(), profile_viewer()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            author_url(&rows[0]),
            Some(format!("/user/{author_id}").as_str()),
            "got {rows:?}"
        );
    });
}

/// A deleted author's row: the anonymous display name, never a link.
#[test]
fn a_row_with_a_deleted_author_shows_the_anonymous_name() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowauthor{}", Uuid::now_v7().simple());
        create_item(app, &marker, "deleted", ANONYMOUS_USER_ID).await;

        let rows = gather_rows(app, &marker, Vec::new(), profile_viewer()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(author_name(&rows[0]), Some("anonymous"), "got {rows:?}");
        assert_eq!(
            author_url(&rows[0]),
            None,
            "the sentinel account is never linked: {rows:?}"
        );
    });
}

/// A gather projecting an explicit field list that omits `author_id` has
/// nothing to resolve, and gets no `author` rather than a wrong one — the
/// same rule `resolve_row_urls` applies to a row with no `id`.
#[test]
fn a_row_without_an_author_id_gets_no_author() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowauthor{}", Uuid::now_v7().simple());
        create_item(app, &marker, "titleonly", ANONYMOUS_USER_ID).await;

        let rows = gather_rows(
            app,
            &marker,
            vec![QueryField {
                field_name: "title".to_string(),
                table_alias: None,
                label: None,
            }],
            profile_viewer(),
        )
        .await;

        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].get("author_id").is_none(),
            "fixture assumption: this projection omits author_id"
        );
        assert!(rows[0].get("author").is_none(), "got {:?}", rows[0]);
    });
}

/// A mixed page is resolved as a set: every row in a mixed page gets its own
/// right answer from the one lookup, same guarantee `resolve_row_urls` makes
/// for `url`.
#[test]
fn a_mixed_result_set_resolves_every_row() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("rowauthor{}", Uuid::now_v7().simple());
        let name = format!("b166gathermixed-{}", Uuid::now_v7().simple());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let real_author: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .expect("author id");

        create_item(app, &marker, "real", real_author).await;
        create_item(app, &marker, "deleted", ANONYMOUS_USER_ID).await;

        let rows = gather_rows(app, &marker, Vec::new(), plain_viewer()).await;
        assert_eq!(rows.len(), 2);

        let names: Vec<&str> = rows.iter().filter_map(author_name).collect();
        assert_eq!(names.len(), 2, "every row carries an author: {rows:?}");
        assert!(names.contains(&name.as_str()), "got {names:?}");
        assert!(names.contains(&"anonymous"), "got {names:?}");
    });
}

// -------------------------------------------------------------------------
// Query count: one query for the whole page, never one per row
// -------------------------------------------------------------------------

struct Events(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Events {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.0
            .lock()
            .unwrap()
            .push(event.metadata().target().to_string());
    }
}

/// `run`'s own result, and the number of `sqlx::query` statements it issued.
async fn statement_count<F: std::future::Future<Output = T>, T>(run: F) -> (T, usize) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(Events(seen.clone()));
    let result = run.with_subscriber(subscriber).await;
    let count = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|t| *t == "sqlx::query")
        .count();
    (result, count)
}

/// The defect this guards against: a per-row author lookup grows with the
/// row count. A 20-row gather costs the same statements whether one author
/// wrote every row or each row has its own.
#[test]
fn a_gather_costs_the_same_queries_for_one_author_as_for_twenty() {
    run_test(async {
        let app = shared_app().await;

        let one_marker = format!("rowauthorcount{}", Uuid::now_v7().simple());
        let one_author = format!("b166gathercount-one-{}", Uuid::now_v7().simple());
        app.create_test_user(
            &one_author,
            "test-password-123",
            &format!("{one_author}@example.com"),
        )
        .await;
        let one_author_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&one_author)
            .fetch_one(&app.db)
            .await
            .expect("author id");
        for n in 1..=20 {
            create_item(app, &one_marker, &format!("{n}"), one_author_id).await;
        }

        let (one_rows, one_count) =
            statement_count(gather_rows(app, &one_marker, Vec::new(), profile_viewer())).await;
        assert_eq!(one_rows.len(), 20);

        let many_marker = format!("rowauthorcount{}", Uuid::now_v7().simple());
        for n in 1..=20 {
            let name = format!("b166gathercount-many-{n}-{}", Uuid::now_v7().simple());
            app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
                .await;
            let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
                .bind(&name)
                .fetch_one(&app.db)
                .await
                .expect("author id");
            create_item(app, &many_marker, &format!("{n}"), id).await;
        }

        let (many_rows, many_count) =
            statement_count(gather_rows(app, &many_marker, Vec::new(), profile_viewer())).await;
        assert_eq!(many_rows.len(), 20);

        assert_eq!(
            one_count, many_count,
            "a gather's queries must not grow with its row count: {one_count} for one \
             author, {many_count} for twenty distinct authors"
        );
    });
}

// -------------------------------------------------------------------------
// The shipped templates
// -------------------------------------------------------------------------

fn render_shipped(relative: &str, context: tera::Context) -> String {
    let path = common::project_root().join(relative);
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let mut tera = tera::Tera::default();
    tera.register_filter(
        "format_date",
        |value: &tera::Value, _: &HashMap<String, tera::Value>| Ok(value.clone()),
    );
    tera.register_filter(
        "plain_text",
        |value: &tera::Value, _: &HashMap<String, tera::Value>| Ok(value.clone()),
    );
    tera.add_raw_template("shipped", &source)
        .expect("the shipped template parses");
    tera.render("shipped", &context)
        .expect("the shipped template renders")
}

#[test]
fn the_shipped_row_template_prints_the_author_as_a_link() {
    let mut context = tera::Context::new();
    context.insert(
        "row",
        &serde_json::json!({
            "id": "0193a5a0-0001-7000-8000-000000000042",
            "title": "Hello",
            "url": "/blog/hello",
            "author": { "name": "Jeremy", "url": "/user/0193a5a0-0001-7000-8000-000000000099" },
        }),
    );

    let html = render_shipped("templates/gather/row.html", context);
    assert!(
        html.contains("<a href=\"/user/0193a5a0-0001-7000-8000-000000000099\">Jeremy</a>"),
        "got {html}"
    );
}

#[test]
fn the_shipped_row_template_prints_an_unlinked_author_without_a_link() {
    let mut context = tera::Context::new();
    context.insert(
        "row",
        &serde_json::json!({
            "id": "0193a5a0-0001-7000-8000-000000000042",
            "title": "Hello",
            "url": "/blog/hello",
            "author": { "name": "Jeremy" },
        }),
    );

    let html = render_shipped("templates/gather/row.html", context);
    // The exact substring proves no link wraps the name: a linked author
    // would read `by <a href="...">Jeremy</a>`, breaking this match.
    assert!(
        html.contains("gather-row__author\">by Jeremy<"),
        "got {html}"
    );
}

#[test]
fn the_shipped_row_template_renders_without_an_author() {
    let mut context = tera::Context::new();
    context.insert(
        "row",
        &serde_json::json!({
            "id": "0193a5a0-0001-7000-8000-000000000042",
            "title": "Hello",
        }),
    );

    let html = render_shipped("templates/gather/row.html", context);
    assert!(!html.contains("gather-row__author"), "got {html}");
}
