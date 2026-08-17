#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin serves an HTTP request and writes its own table (**K1 fix 1**,
//! G-NO-PLUGIN-HTTP).
//!
//! The finding this closes, stated as Argus M3 stated it: *there was no surface
//! in Trovato 1.0 through which a plugin served a request, and therefore no way
//! for an authenticated user to write a plugin-owned table.* `tap_menu`'s
//! `callback` was dropped on deserialize, the form taps were never dispatched
//! from any route, `tap_form_ajax` was admin-only and service-less, and
//! `public_functions` is plugin-to-plugin only.
//!
//! This drives the **real** `plugins/test_plugin_api` wasm through the real
//! router over HTTP: an anonymous caller is refused, a permitted reader POSTs,
//! the row lands in a plugin-owned table, and the reader reads it back.
//!
//! `test_plugin_api` is `default_enabled = false`, so this file installs it in
//! the database and builds its **own** `TestApp` — `AppState` resolves its
//! enabled plugin set, and therefore the routes `tap_menu` declares, at
//! construction. It disables it again on the way out so the setting does not
//! leak into other test binaries.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_plugin_api/`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use uuid::Uuid;

const PLUGIN: &str = "test_plugin_api";
/// Enabled alongside the fixture so its own `api` routes register, which is
/// what the consumer-validation test below exercises.
const ARGUS: &str = "argus";
const PERM_WRITE: &str = "write test notes";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("plugin api fixture app init thread panicked")
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
    for plugin in [PLUGIN, ARGUS] {
        trovato_kernel::plugin::status::install_plugin(&pool, plugin, "1.0.0")
            .await
            .unwrap_or_else(|e| panic!("failed to install '{plugin}': {e:#}"));
    }
    pool.close().await;

    // Integration tests run with the crate directory as CWD, so `Config`'s
    // default `./plugins` resolves to nothing. Point it at the real directory
    // through the config rather than through `PLUGINS_DIR`: `plugins_dirs` is a
    // `Config` field, so this fixture needs no process-global write at all. The
    // environment still wins when it says something, as it did before.
    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Grant `PERM_WRITE` to one user, through a role of one so the grant does not
/// widen anyone else's access.
async fn grant_write_permission(app: &TestApp, user: Uuid) {
    grant_permission(app, user, PERM_WRITE).await;
}

/// Grant one permission to one user, through a role of one.
async fn grant_permission(app: &TestApp, user: Uuid, permission: &str) {
    let role_id = Uuid::now_v7();
    let role = format!("k1_api_role_{user}");
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (id, name) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
    )
    .bind(role_id)
    .bind(&role)
    .fetch_one(&app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(role_id)
    .bind(permission)
    .execute(&app.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(user)
        .bind(role_id)
        .execute(&app.db)
        .await
        .unwrap();
    // The permission set is cached per user, and the login that precedes this
    // has already populated it.
    app.state.permissions().invalidate_user(user);
}

async fn user_id_of(app: &TestApp, username: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// Read a fresh CSRF token for a logged-in session.
///
/// The login page renders one and is reachable by any session, admin or not —
/// which matters here, because the point of these tests is that a **plain
/// reader** can write, so they must not need an admin-only page to get a token.
async fn csrf_token(app: &TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/user/login").body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes);
    for needle in [
        "name=\"csrf_token\"",
        "name=\"_token\"",
        "name=\"token\"",
        "name=\"_csrf\"",
    ] {
        if let Some(pos) = html.find(needle)
            && let Some(rel) = html[pos..].find("value=\"")
        {
            let start = pos + rel + 7;
            let end = html[start..].find('"').map(|p| start + p).unwrap_or(start);
            if end > start {
                return html[start..end].to_string();
            }
        }
    }
    panic!(
        "no CSRF token in the login page: {}",
        &html[..html.len().min(400)]
    );
}

/// Delete only this test's own rows.
///
/// The tests in this file share one app and run in parallel, so a blanket
/// `DELETE FROM tpa_notes` would wipe a sibling's fixture mid-assertion.
async fn cleanup(app: &TestApp, user: Uuid) {
    sqlx::query("DELETE FROM tpa_notes WHERE user_id = $1")
        .bind(user)
        .execute(&app.db)
        .await
        .ok();
}

/// How many notes carry a slug, across all users — for the assertions whose
/// claim is "nothing was written at all".
async fn note_count(app: &TestApp, slug: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tpa_notes WHERE slug = $1")
        .bind(slug)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// Leave the fixture disabled so it does not load in other test binaries.
async fn disable_plugin(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = ANY($1)")
        .bind(vec![PLUGIN.to_string(), ARGUS.to_string()])
        .execute(&app.db)
        .await
        .ok();
}

/// **The finding, closed.** An authenticated reader POSTs to a plugin-declared
/// route and a row lands in a plugin-owned table — the write that had nowhere
/// to go.
#[test]
fn an_authenticated_caller_writes_a_plugin_owned_table() {
    common::run_test(async {
        let app = app();

        let cookies = app
            .create_and_login_user(
                "k1apiwriter",
                "correct-horse-battery-staple",
                "k1w@test.local",
            )
            .await;
        let user = user_id_of(app, "k1apiwriter").await;
        grant_write_permission(app, user).await;
        cleanup(app, user).await;
        let token = csrf_token(app, &cookies).await;

        let response = app
            .request_with_cookies(
                Request::post("/tpa/note/first?source=test")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(r#"{"text":"hello from a reader"}"#))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the plugin route must serve the write: {body}"
        );

        assert_eq!(body["written"], true);
        // The whole request context crossed the boundary: the path parameter,
        // the query string, and the authenticated user's id.
        assert_eq!(body["slug"], "first");
        assert_eq!(body["user_id"], user.to_string());
        assert_eq!(body["query"]["source"], "test");

        // And the row is really in the plugin's own table.
        let (text, method): (String, String) = sqlx::query_as(
            "SELECT text, method FROM tpa_notes WHERE user_id = $1 AND slug = 'first'",
        )
        .bind(user)
        .fetch_one(&app.db)
        .await
        .expect("the plugin wrote its table");
        assert_eq!(text, "hello from a reader");
        assert_eq!(method, "POST");

        // The reader reads it back through the plugin's own GET route.
        let response = app
            .request_with_cookies(
                Request::get("/tpa/notes").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let rows = json_body(response).await;
        assert_eq!(rows.as_array().map(Vec::len), Some(1), "got {rows}");
        assert_eq!(rows[0]["slug"], "first");

        // Re-POSTing the same slug updates rather than duplicating, so a
        // retried request is safe. A CSRF token is single-use, so this needs a
        // fresh one — which is exactly the round-trip tax the bearer exemption
        // below removes for a token client.
        let token = csrf_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post("/tpa/note/first")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(r#"{"text":"edited"}"#))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tpa_notes WHERE user_id = $1")
            .bind(user)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 1, "the write is idempotent on (user, slug)");

        cleanup(app, user).await;
        disable_plugin(app).await;
    });
}

/// The permission on the menu entry is a real gate: an anonymous caller is
/// refused **before** the plugin is dispatched, and writes nothing.
#[test]
fn an_anonymous_caller_is_refused_and_writes_nothing() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(
                Request::post("/tpa/note/anon-forged")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"should not land"}"#))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an anonymous caller lacking the permission must be refused"
        );

        assert_eq!(
            note_count(app, "anon-forged").await,
            0,
            "nothing may be written by a refused caller"
        );

        disable_plugin(app).await;
    });
}

/// An authenticated caller *without* the permission is refused too — the gate
/// is the permission, not merely being logged in.
#[test]
fn an_authenticated_caller_without_the_permission_is_refused() {
    common::run_test(async {
        let app = app();

        let cookies = app
            .create_and_login_user(
                "k1apireader",
                "correct-horse-battery-staple",
                "k1r@test.local",
            )
            .await;
        let token = csrf_token(app, &cookies).await;

        let response = app
            .request_with_cookies(
                Request::post("/tpa/note/nope")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(r#"{"text":"x"}"#))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // The public GET route on the same plugin still serves them, proving
        // the gate is per-entry rather than per-plugin.
        let response = app
            .request_with_cookies(
                Request::get("/tpa/notes").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        disable_plugin(app).await;
    });
}

/// A cookie-authenticated write with no CSRF token is refused. Widening the
/// surface must not open a forgery hole.
#[test]
fn a_cookie_authenticated_write_without_a_csrf_token_is_refused() {
    common::run_test(async {
        let app = app();

        let cookies = app
            .create_and_login_user(
                "k1apicsrf",
                "correct-horse-battery-staple",
                "k1c@test.local",
            )
            .await;
        let user = user_id_of(app, "k1apicsrf").await;
        grant_write_permission(app, user).await;

        for headers in [None, Some("not-a-real-token")] {
            let mut request =
                Request::post("/tpa/note/forged").header(header::CONTENT_TYPE, "application/json");
            if let Some(token) = headers {
                request = request.header("X-CSRF-Token", token);
            }
            let response = app
                .request_with_cookies(
                    request.body(Body::from(r#"{"text":"x"}"#)).unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "a cookie-authenticated write needs a valid CSRF token"
            );
        }

        assert_eq!(note_count(app, "csrf-forged").await, 0);

        cleanup(app, user).await;
        disable_plugin(app).await;
    });
}

/// **The CSRF/bearer posture (G-CSRF-NO-BEARER-BYPASS), decided and proven.**
///
/// A bearer-authenticated write needs no CSRF token: a browser never attaches a
/// bearer token to a cross-site request by itself, so there is no forgery to
/// protect against. This is what lets a token client write without a
/// session-establishing round-trip.
#[test]
fn a_bearer_authenticated_write_needs_no_csrf_token() {
    common::run_test(async {
        let app = app();

        app.create_test_user(
            "k1apibearer",
            "correct-horse-battery-staple",
            "k1b@test.local",
        )
        .await;
        let user = user_id_of(app, "k1apibearer").await;
        grant_write_permission(app, user).await;
        cleanup(app, user).await;

        // Mint an API token for the user, as the token admin surface does.
        let (_token, raw) = trovato_kernel::models::api_token::ApiToken::create(
            &app.db,
            user,
            "k1 test token",
            None,
        )
        .await
        .expect("mint api token");
        assert!(!raw.is_empty());

        // No cookies, no CSRF header — only the token.
        let response = app
            .request(
                Request::post("/tpa/note/from-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {raw}"))
                    .body(Body::from(r#"{"text":"from a token client"}"#))
                    .unwrap(),
            )
            .await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a bearer-authenticated write must not need a CSRF round-trip: {body}"
        );

        let text: String = sqlx::query_scalar(
            "SELECT text FROM tpa_notes WHERE user_id = $1 AND slug = 'from-token'",
        )
        .bind(user)
        .fetch_one(&app.db)
        .await
        .expect("the token client's write landed");
        assert_eq!(text, "from a token client");

        cleanup(app, user).await;
        disable_plugin(app).await;
    });
}

/// **The Argus consumer validation (M3 deviation 5, un-deviated).** An
/// authenticated reader POSTs an upvote to the real `plugins/argus` route and
/// reads it back. M3 shipped `argus_reactions` with a schema, indexes, storage
/// functions and unit tests, and **no writer** — an upvote had nowhere to go.
///
/// Runs against the same app: `argus` is `default_enabled`, so its `tap_menu`
/// entries are registered here too.
#[test]
fn an_argus_reader_can_post_a_reaction_and_read_it_back() {
    common::run_test(async {
        let app = app();

        let cookies = app
            .create_and_login_user(
                "k1argusreader",
                "correct-horse-battery-staple",
                "k1a@test.local",
            )
            .await;
        let user = user_id_of(app, "k1argusreader").await;
        grant_permission(app, user, "react to argus stories").await;

        // A story to react to. Argus stories are Items, so this is one.
        let story = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO item (id, type, title, fields, status, author_id, created, changed) \
             VALUES ($1, 'argus_story', 'A story worth an upvote', '{}'::jsonb, 1, $2, 0, 0)",
        )
        .bind(story)
        .bind(user)
        .execute(&app.db)
        .await
        .expect("seed an argus_story item");

        let token = csrf_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/argus/story/{story}/react"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(r#"{"reaction":"upvote"}"#))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(status, StatusCode::OK, "the upvote must be served: {body}");
        assert_eq!(body["reactions"], serde_json::json!(["upvote"]));

        // The row is in Argus's own table.
        let kind: String = sqlx::query_scalar(
            "SELECT reaction_type FROM argus_reactions WHERE user_id = $1 AND story_item_id = $2",
        )
        .bind(user)
        .bind(story)
        .fetch_one(&app.db)
        .await
        .expect("the reaction landed in argus_reactions");
        assert_eq!(kind, "upvote");

        // And the reader reads it back through Argus's own GET route.
        let response = app
            .request_with_cookies(
                Request::get(format!("/argus/story/{story}/reactions"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await["reactions"],
            serde_json::json!(["upvote"])
        );

        sqlx::query("DELETE FROM argus_reactions WHERE user_id = $1")
            .bind(user)
            .execute(&app.db)
            .await
            .ok();
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(story)
            .execute(&app.db)
            .await
            .ok();
        disable_plugin(app).await;
    });
}

/// Only `handler_type = "api"` entries are routed here. A `page` entry declared
/// by the same plugin is not served by the plugin-api router.
#[test]
fn a_page_menu_entry_is_not_served_as_an_api() {
    common::run_test(async {
        let app = app();
        let response = app
            .request(Request::get("/tpa/page").body(Body::empty()).unwrap())
            .await;
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "a page entry must not be dispatched to tap_api"
        );
        disable_plugin(app).await;
    });
}
