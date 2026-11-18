#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The admin edit form asks the access seam before it renders.
//!
//! `GET /admin/content/{id}/edit` checked `edit any content` and rendered the
//! form, never asking `ItemService::check_access(item, "edit", ..)`. A plugin's
//! Deny on edit was therefore ignored when the form rendered, and honoured only
//! when it was submitted (through `ItemService::update`). The form showed a user
//! an item's stored values that the seam had decided they may not edit.
//!
//! The fixture `test_item_deny` answers `tap_item_access` with Deny for `edit`
//! on every item of type `test_edit_denied`, and is neutral otherwise.
//!
//! Requires Postgres, Redis and the fixture's `.wasm` (built by CI before the
//! test job).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test};
use trovato_kernel::models::{CreateItem, Item};
use uuid::Uuid;

const PLUGIN: &str = "test_item_deny";
const DENIED_TYPE: &str = "test_edit_denied";

/// Serializes seeding of the type across binaries and shards.
const DENIED_TYPE_SEED_LOCK: i64 = 0x_B690_0000_0001;

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("edit form access app init thread panicked")
    })
}

/// Enable the fixture, build an app that loads it, and disable it again so no
/// other app built later loads it.
async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));

    let app = TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await;

    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&pool)
        .await
        .expect("disable the fixture plugin");
    pool.close().await;

    seed_denied_type(&app).await;
    app
}

/// The type the fixture denies edit on. A permanent fixture: found, not made
/// again, on a second run.
async fn seed_denied_type(app: &TestApp) {
    let settings = serde_json::json!({ "fields": [], "title_label": "Title" });
    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(DENIED_TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");
    sqlx::query(
        r#"INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings)
           VALUES ($1, 'Edit denied', 'Items a fixture plugin denies edit on', true, 'Title', 'core', $2)
           ON CONFLICT (type) DO NOTHING"#,
    )
    .bind(DENIED_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed the denied type");
    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .replace(
            DENIED_TYPE,
            "Edit denied",
            Some("Items a fixture plugin denies edit on"),
            settings,
        )
        .await
        .ok();
}

/// An item of `item_type`, deleted when the test ends.
async fn item_of_type(app: &TestApp, item_type: &str, author_id: Uuid) -> Item {
    let item = Item::create(
        &app.db,
        CreateItem {
            item_type: item_type.to_string(),
            title: format!("b69 edit form {}", Uuid::now_v7().simple()),
            author_id,
            status: Some(1),
            promote: None,
            sticky: None,
            fields: None,
            stage_id: None,
            language: None,
            log: None,
        },
    )
    .await
    .expect("create item");
    let pool = app.db.clone();
    let id = item.id;
    common::defer_cleanup(move || async move {
        let _ = Item::delete(&pool, id).await;
    });
    item
}

async fn get_edit_form(app: &TestApp, item_id: Uuid, cookies: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(format!("/admin/content/{item_id}/edit"))
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// A plugin's Deny on edit makes the edit form refuse, as the submit does: a
/// 403, and the item's stored title is not rendered into a form.
#[test]
fn a_plugin_deny_on_edit_makes_the_edit_form_refuse() {
    run_test(async {
        let app = app();
        let (editor_id, cookies) =
            common::user_holding(app, "b69ed", &["edit any content", "access content"]).await;
        let item = item_of_type(app, DENIED_TYPE, editor_id).await;

        let (status, html) = get_edit_form(app, item.id, &cookies).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a plugin's Deny on edit must make the edit form refuse"
        );
        assert!(
            !html.contains(&item.title),
            "the refused form must not render the item's values"
        );
    });
}

/// Guard: the fixture is neutral about every other type, so the same editor
/// still gets the form for a page. Without this the test above would pass for a
/// form that refused everyone.
#[test]
fn the_edit_form_still_renders_where_nothing_denies() {
    run_test(async {
        let app = app();
        let (editor_id, cookies) =
            common::user_holding(app, "b69ed", &["edit any content", "access content"]).await;
        let item = item_of_type(app, "page", editor_id).await;

        let (status, html) = get_edit_form(app, item.id, &cookies).await;
        assert_eq!(status, StatusCode::OK, "the edit form renders for a page");
        assert!(html.contains(&item.title), "the form carries the item");
    });
}
