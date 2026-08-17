#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The record view route serves whatever key column the record type declares.
//!
//! A `[[record_types]]` declaration names its own `id_column`, and the kernel
//! reads that column everywhere else — the list route projects it, gather
//! resolves the logical `id` to it. The view route used to be the exception: it
//! extracted the path segment as `Path<(String, uuid::Uuid)>`, so axum rejected
//! the request with a 400 before the handler ran for any record type whose key
//! is not a uuid. A bigint-keyed record type listed fine and 400'd the instant
//! you opened a row.
//!
//! These tests drive the **real** router over the **real** registry, with the
//! reference plugin's two record types: `event_record` (uuid key) and
//! `legacy_record` (bigint key). One route serves both.
//!
//! `trovato_record_ref` is `default_enabled = false` and `AppState` loads only
//! enabled plugins, so this file installs it in the database and builds its
//! **own** `TestApp` — the record-type registry is resolved at construction. It
//! disables it again on the way out so the setting does not leak into other test
//! binaries.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/trovato_record_ref/`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use common::TestApp;
use uuid::Uuid;

const PLUGIN: &str = "trovato_record_ref";
/// The uuid-keyed record type (`record_event.id UUID`).
const UUID_TYPE: &str = "event_record";
/// The bigint-keyed record type (`record_legacy.id BIGINT`).
const BIGINT_TYPE: &str = "legacy_record";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("record view fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;

    // Integration tests run with the crate directory as CWD, so `Config`'s
    // default `./plugins` resolves to nothing. Point it at the real directory
    // through the config rather than through `PLUGINS_DIR`, so this fixture
    // needs no process-global write.
    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

/// Leave the fixture disabled so it does not load in other test binaries.
async fn disable_plugin(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&app.db)
        .await
        .ok();
}

/// Create the fixture's backing tables from its own migration files.
///
/// `AppState` already runs them for an enabled plugin; running them here too
/// (both are `CREATE TABLE IF NOT EXISTS`) keeps these tests independent of the
/// migration-tracking state a shared database carries over from earlier runs.
async fn ensure_tables(app: &TestApp) {
    for file in [
        "migrations/001_create_event_record.sql",
        "migrations/002_create_legacy_record.sql",
    ] {
        let sql = std::fs::read_to_string(
            common::project_root()
                .join("plugins")
                .join(PLUGIN)
                .join(file),
        )
        .unwrap_or_else(|e| panic!("read {file}: {e}"));
        sqlx::query(&sql)
            .execute(&app.db)
            .await
            .unwrap_or_else(|e| panic!("apply {file}: {e}"));
    }
}

/// Seed one bigint-keyed row. Ids are chosen above `u32::MAX` so a truncation to
/// a narrower integer type could not pass unnoticed.
async fn seed_legacy(app: &TestApp, id: i64, title: &str) {
    ensure_tables(app).await;
    sqlx::query(
        "INSERT INTO record_legacy (id, title, created, changed) \
         VALUES ($1, $2, 1700000000, 1700000000) \
         ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title",
    )
    .bind(id)
    .bind(title)
    .execute(&app.db)
    .await
    .expect("seed record_legacy");
}

/// Seed one uuid-keyed row.
async fn seed_event(app: &TestApp, id: Uuid, title: &str) {
    ensure_tables(app).await;
    sqlx::query(
        "INSERT INTO record_event \
         (id, title, author_id, published, location, capacity, secret_notes, created, changed) \
         VALUES ($1, $2, NULL, true, 'Barga', 200, 'classified', 1700000000, 1700000000) \
         ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title",
    )
    .bind(id)
    .bind(title)
    .execute(&app.db)
    .await
    .expect("seed record_event");
}

async fn body_text(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn view(app: &TestApp, cookies: &str, type_name: &str, id: &str) -> Response {
    app.request_with_cookies(
        Request::get(format!("/admin/structure/records/{type_name}/{id}"))
            .body(Body::empty())
            .unwrap(),
        cookies,
    )
    .await
}

/// **The bug, closed.** A record type keyed by a bigint opens its row. This
/// returned 400 before the handler ever ran, because the id was extracted as a
/// `uuid::Uuid`.
#[test]
fn bigint_keyed_record_opens_by_id() {
    common::run_test(async {
        let app = app();
        seed_legacy(app, 4_294_967_301, "Legacy Row One").await;

        let cookies = app
            .create_and_login_admin(
                "recviewadmin1",
                "correct-horse-battery-staple",
                "rv1@test.local",
            )
            .await;

        let response = view(app, &cookies, BIGINT_TYPE, "4294967301").await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(
            body.contains("Legacy Row One"),
            "bigint-keyed record did not render its row: {body}"
        );

        disable_plugin(app).await;
    });
}

/// A uuid-keyed record type still opens — the general path did not cost the
/// case that already worked.
#[test]
fn uuid_keyed_record_still_opens() {
    common::run_test(async {
        let app = app();
        let id = Uuid::now_v7();
        seed_event(app, id, "Uuid Keyed Event").await;

        let cookies = app
            .create_and_login_admin(
                "recviewadmin2",
                "correct-horse-battery-staple",
                "rv2@test.local",
            )
            .await;

        let response = view(app, &cookies, UUID_TYPE, &id.to_string()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(
            body.contains("Uuid Keyed Event"),
            "uuid-keyed record did not render its row: {body}"
        );

        disable_plugin(app).await;
    });
}

/// The uuid extractor accepted any spelling `Uuid::parse_str` accepts, and the
/// text comparison keeps that: an uppercase id still opens the row Postgres
/// renders in lowercase.
#[test]
fn uuid_keyed_record_opens_from_an_uppercase_id() {
    common::run_test(async {
        let app = app();
        let id = Uuid::now_v7();
        seed_event(app, id, "Uppercase Spelling Event").await;

        let cookies = app
            .create_and_login_admin(
                "recviewadmin3",
                "correct-horse-battery-staple",
                "rv3@test.local",
            )
            .await;

        let response = view(app, &cookies, UUID_TYPE, &id.to_string().to_uppercase()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(
            body.contains("Uppercase Spelling Event"),
            "uppercase uuid did not render its row: {body}"
        );

        disable_plugin(app).await;
    });
}

/// A bigint key with no matching row renders not-found, not an error — and not
/// the 400 the extractor used to produce for the whole type.
#[test]
fn missing_bigint_id_renders_not_found() {
    common::run_test(async {
        let app = app();
        ensure_tables(app).await;

        let cookies = app
            .create_and_login_admin(
                "recviewadmin4",
                "correct-horse-battery-staple",
                "rv4@test.local",
            )
            .await;

        let response = view(app, &cookies, BIGINT_TYPE, "4294967399").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        disable_plugin(app).await;
    });
}

/// A path segment that is not a number at all, on a bigint-keyed type, is a
/// miss rather than a database error: the comparison is `id::text = $1`, so
/// there is no cast for the value to fail.
#[test]
fn non_numeric_id_on_a_bigint_key_renders_not_found() {
    common::run_test(async {
        let app = app();
        ensure_tables(app).await;

        let cookies = app
            .create_and_login_admin(
                "recviewadmin5",
                "correct-horse-battery-staple",
                "rv5@test.local",
            )
            .await;

        let response = view(app, &cookies, BIGINT_TYPE, "not-a-key").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        disable_plugin(app).await;
    });
}

/// The admin guard still gates the route. Worth asserting on the bigint path
/// specifically: extraction ran before the guard, so this request used to be
/// refused with a 400 rather than by the guard at all.
#[test]
fn view_route_still_requires_an_admin() {
    common::run_test(async {
        let app = app();
        seed_legacy(app, 4_294_967_302, "Guarded Legacy Row").await;

        let anonymous = view(app, "", BIGINT_TYPE, "4294967302").await;
        assert!(
            anonymous.status().is_redirection(),
            "anonymous request was not redirected: {}",
            anonymous.status()
        );
        let location = anonymous
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(location, "/user/login");

        let cookies = app
            .create_and_login_user(
                "recviewreader",
                "correct-horse-battery-staple",
                "rvr@test.local",
            )
            .await;
        let forbidden = view(app, &cookies, BIGINT_TYPE, "4294967302").await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

        disable_plugin(app).await;
    });
}

/// An unregistered record type is not found whatever the id looks like — the
/// registry lookup, not the id, decides.
#[test]
fn unknown_record_type_renders_not_found() {
    common::run_test(async {
        let app = app();

        let cookies = app
            .create_and_login_admin(
                "recviewadmin6",
                "correct-horse-battery-staple",
                "rv6@test.local",
            )
            .await;

        let response = view(app, &cookies, "no_such_record_type", "12345").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        disable_plugin(app).await;
    });
}
