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
use trovato_kernel::file::FileStatus;
use uuid::Uuid;

const PLUGIN: &str = "test_plugin_api";
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
    common::ensure_database_migrated(&database_url).await;
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
    let role = format!("k1_api_role_{user}");
    let role_id = common::create_test_role(app, &role).await;
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
        .bind(vec![PLUGIN.to_string()])
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
        // The AppError body every kernel route answers with, plus the `error`
        // key plugin clients read before the shapes were unified, carrying the
        // same string as `message` until 2.0.
        let body = json_body(response).await;
        assert_eq!(body["code"], "unauthorized", "{body}");
        assert!(body["request_id"].is_string(), "{body}");
        assert!(body["message"].is_string(), "{body}");
        assert_eq!(body["error"], body["message"], "{body}");

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

/// Read the response body as text.
async fn text_body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Pull `value="…"` out of the hidden `_token` input a plugin rendered.
fn token_from_form(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .unwrap_or_else(|| panic!("no _token input in the plugin's form: {html}"))
        + needle.len();
    let end = html[start..]
        .find('"')
        .map(|p| start + p)
        .unwrap_or_else(|| panic!("unterminated _token value: {html}"));
    assert!(end > start, "the plugin rendered an empty token: {html}");
    html[start..end].to_string()
}

/// **The no-JavaScript form, end to end, anonymously.**
///
/// A plain `<form method="post">` cannot set a header, so before this a
/// plugin-served form was refused with 403 whatever it rendered. Two things had
/// to be true, and this drives both through the real wasm: the kernel hands the
/// plugin a token to embed (`ApiRequest::csrf_token`), and it accepts that token
/// back from a `_token` field in a form-urlencoded body.
///
/// Anonymous on purpose. A contact form's caller has no account, so the session
/// the token is bound to is one the visitor acquired by asking for the form.
#[test]
fn an_anonymous_visitor_posts_a_plugin_served_form_without_javascript() {
    common::run_test(async {
        let app = app();

        // GET the form with no session at all, the way a visitor arrives.
        let response = app
            .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the form is public and must render for an anonymous visitor"
        );
        let cookies = common::extract_cookies(&response);
        assert!(
            !cookies.is_empty(),
            "rendering a token has to establish the session it is bound to"
        );
        let html = text_body(response).await;
        let token = token_from_form(&html);

        // POST it back exactly as a browser with scripting disabled would: form
        // encoding, the token in the body, and no X-CSRF-Token header anywhere.
        let response = app
            .request_with_cookies(
                Request::post("/tpa/form")
                    .header(
                        header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded; charset=UTF-8",
                    )
                    .body(Body::from(format!(
                        "_token={token}&message=hello+from+a+plain+form"
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = text_body(response).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a _token field must be accepted in place of the header: {body}"
        );
        assert!(
            body.contains("message=hello+from+a+plain+form"),
            "the plugin must have been dispatched with the body: {body}"
        );

        disable_plugin(app).await;
    });
}

/// The field is not a way around the check. A forged or absent `_token` is
/// refused exactly as a forged or absent header is.
#[test]
fn a_form_post_with_a_bad_or_missing_token_is_refused() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
            .await;
        let cookies = common::extract_cookies(&response);
        let real_token = token_from_form(&text_body(response).await);

        for body in [
            String::new(),
            "message=x".to_string(),
            "_token=&message=x".to_string(),
            "_token=not-a-real-token&message=x".to_string(),
            // A token from somebody else's session is somebody else's token.
            format!("_token={}&message=x", "0".repeat(64)),
            // Two tokens in one body is malformed, not a coin flip, even when
            // one of them is the honest one.
            format!("_token={real_token}&_token=forged&message=x"),
        ] {
            let response = app
                .request_with_cookies(
                    Request::post("/tpa/form")
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .body(Body::from(body.clone()))
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "this body must not be accepted: {body:?}"
            );
        }

        disable_plugin(app).await;
    });
}

/// A token is single-use, and the fallback does not change that. The same body
/// replayed is refused the second time.
#[test]
fn a_form_token_cannot_be_replayed() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
            .await;
        let cookies = common::extract_cookies(&response);
        let token = token_from_form(&text_body(response).await);

        let post = || {
            app.request_with_cookies(
                Request::post("/tpa/form")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("_token={token}&message=x")))
                    .unwrap(),
                &cookies,
            )
        };

        assert_eq!(post().await.status(), StatusCode::OK);
        assert_eq!(
            post().await.status(),
            StatusCode::FORBIDDEN,
            "a spent token must not verify a second time"
        );

        disable_plugin(app).await;
    });
}

/// The body is only read when the content type says it is a form. A JSON caller
/// that puts a token in its body still needs the header, so the fallback did not
/// quietly widen the check to every request shape.
#[test]
fn a_json_body_carrying_a_token_field_is_not_accepted() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
            .await;
        let cookies = common::extract_cookies(&response);
        let token = token_from_form(&text_body(response).await);

        let response = app
            .request_with_cookies(
                Request::post("/tpa/form")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(r#"{{"_token":"{token}"}}"#)))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a JSON body is not a form and must not be scanned for a token"
        );

        // And the same token still works through the header, which proves the
        // refusal above was about the content type rather than about the token
        // having been spent.
        let response = app
            .request_with_cookies(
                Request::post("/tpa/form")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from("{}"))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        disable_plugin(app).await;
    });
}

/// The value of the hidden input `name` in `html`.
fn hidden_value(html: &str, name: &str) -> String {
    let needle = format!(r#"name="{name}" value=""#);
    let start = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no {name} in: {html}"))
        + needle.len();
    let end = start + html[start..].find('"').unwrap();
    html[start..end].to_string()
}

/// GET `/tpa/pipeline`: the session, the page, its form's token and build id.
async fn load_pipeline_form(app: &TestApp) -> (String, String, String, String) {
    let response = app
        .request(Request::get("/tpa/pipeline").body(Body::empty()).unwrap())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookies = common::extract_cookies(&response);
    let html = text_body(response).await;
    let token = hidden_value(&html, "_token");
    let build_id = hidden_value(&html, "_form_build_id");
    (cookies, html, token, build_id)
}

/// POST `body` form-encoded to `/tpa/pipeline` under `cookies`.
async fn post_pipeline(app: &TestApp, cookies: &str, body: String) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::post("/tpa/pipeline")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, text_body(response).await)
}

/// **The plugin form seam (B83b).** A plugin describes a form in
/// `ApiResponse::form`; the kernel builds it through `FormService` (token,
/// build id, saved state), draws it with the site's form templates after the
/// plugin's body, and hands the plugin's POST callback only the checked
/// values of the inputs the form rendered.
#[test]
fn a_plugin_form_is_built_drawn_and_checked_by_the_kernel() {
    common::run_test(async {
        let app = app();

        let (cookies, html, token, build_id) = load_pipeline_form(app).await;
        let intro = html.find("tpa-pipeline-intro").expect("the plugin's body");
        let form = html
            .find(r#"<form id="test_plugin_api_note" action="/tpa/pipeline" method="post""#)
            .unwrap_or_else(|| panic!("the kernel's form markup: {html}"));
        assert!(intro < form, "the form follows the body");
        for marker in THEME_CHROME {
            assert!(html.contains(marker), "missing {marker}: {html}");
        }

        let (status, body) = post_pipeline(
            app,
            &cookies,
            format!(
                "_token={token}&_form_build_id={build_id}&message=hi+there&mood=loud&smuggled=x"
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let start = body.find("tpa-pipeline-values").expect("the values");
        let shown = &body[start..start + body[start..].find("</p>").unwrap()];
        assert!(
            shown.contains("&quot;message&quot;:&quot;hi there&quot;"),
            "{shown}"
        );
        assert!(
            shown.contains("&quot;mood&quot;:&quot;loud&quot;"),
            "{shown}"
        );
        for absent in ["_token", "_form_build_id", "smuggled"] {
            assert!(
                !shown.contains(absent),
                "only rendered inputs reach the plugin: {shown}"
            );
        }

        disable_plugin(app).await;
    });
}

/// A submission that fails a check never reaches the plugin's POST callback:
/// the kernel asks the plugin for the page again and shows the form with the
/// error and what was typed, under 422, with a fresh token.
#[test]
fn a_failed_plugin_form_submission_is_shown_again_by_the_kernel() {
    common::run_test(async {
        let app = app();

        let (cookies, _, token, build_id) = load_pipeline_form(app).await;
        let (status, html) = post_pipeline(
            app,
            &cookies,
            format!("_token={token}&_form_build_id={build_id}&message=+&mood=loud"),
        )
        .await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{html}");
        assert!(html.contains("Message is required."), "{html}");
        assert!(
            html.contains("tpa-pipeline-intro"),
            "the page, again: {html}"
        );
        assert!(
            !html.contains("tpa-pipeline-values"),
            "the callback never ran: {html}"
        );
        assert!(
            html.contains(r#"<option value="loud" selected>"#),
            "what was chosen stays chosen: {html}"
        );
        assert_ne!(hidden_value(&html, "_token"), token, "a fresh token");

        disable_plugin(app).await;
    });
}

/// The seam does not loosen the token check: a plugin form posted with no
/// token, a forged one, or two is refused with 403 before anything runs.
#[test]
fn a_plugin_form_post_without_its_token_is_refused() {
    common::run_test(async {
        let app = app();

        let (cookies, _, token, build_id) = load_pipeline_form(app).await;
        for body in [
            format!("_form_build_id={build_id}&message=x"),
            format!("_token=forged&_form_build_id={build_id}&message=x"),
            format!("_token={token}&_token=forged&_form_build_id={build_id}&message=x"),
        ] {
            let (status, html) = post_pipeline(app, &cookies, body.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}: {html}");
        }

        disable_plugin(app).await;
    });
}

/// A build id that names no form saved for this visitor is refused as an
/// expired form, and the plugin hears nothing.
#[test]
fn a_plugin_form_post_naming_no_saved_form_is_refused_as_expired() {
    common::run_test(async {
        let app = app();

        let (cookies, _, token, _) = load_pipeline_form(app).await;
        let (status, html) = post_pipeline(
            app,
            &cookies,
            format!("_token={token}&_form_build_id={}&message=x", Uuid::now_v7()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{html}");
        assert!(html.contains("expired"), "{html}");
        assert!(!html.contains("tpa-pipeline-values"));

        disable_plugin(app).await;
    });
}

/// A themed body that is a render tree is drawn through the theme's element
/// templates, not shown as its JSON (B83b).
#[test]
fn a_themed_render_tree_is_drawn_by_the_theme() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(Request::get("/tpa/tree").body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = text_body(response).await;
        assert!(html.contains("tpa-tree-note"), "{html}");
        assert!(!html.contains("\"#type\""), "not its JSON: {html}");
        assert!(
            !html.contains("<b>a tree"),
            "the value is text, escaped: {html}"
        );

        disable_plugin(app).await;
    });
}

/// Markup only the site's page template produces.
///
/// Asserted on the theme's own structure rather than on the site name, which a
/// plugin could coincidentally contain: `site-header` and the labelled `Main` nav
/// come from `page.html` and nowhere else.
const THEME_CHROME: [&str; 3] = [
    r#"class="site-header""#,
    r#"aria-label="Main""#,
    r#"class="site-branding__name""#,
];

/// **A plugin page renders into the site theme.** `tap_api` output is served
/// verbatim by default, so a public plugin page arrived unstyled and with no
/// navigation, and a plugin could not reproduce either — `page.html` is the
/// theme's, and a site may override it.
#[test]
fn a_plugin_page_that_asks_for_the_theme_is_wrapped_in_it() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(Request::get("/tpa/themed").body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8"),
            "a themed page is HTML whatever the plugin put in content_type"
        );
        let html = text_body(response).await;

        // The plugin's own content survived.
        assert!(
            html.contains(r#"id="plugin-content""#),
            "the plugin's content must be in the page: {html}"
        );
        // Wrapped in the site's document and navigation.
        assert!(
            html.contains("<!DOCTYPE html>"),
            "a themed page is a whole document: {html}"
        );
        for marker in THEME_CHROME {
            assert!(
                html.contains(marker),
                "the site theme must surround it, missing {marker}: {html}"
            );
        }
        // And the title the plugin named reached the document.
        assert!(
            html.contains("A Themed Plugin Page"),
            "the plugin's title must reach the page: {html}"
        );

        disable_plugin(app).await;
    });
}

/// **Theming is opt-in.** The form route does not ask for it and does not get it,
/// which is what keeps every admin screen and JSON endpoint byte identical.
#[test]
fn a_plugin_page_that_does_not_ask_for_the_theme_is_served_raw() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = text_body(response).await;

        assert!(html.contains(r#"name="_token""#), "the plugin's own body");
        for marker in THEME_CHROME {
            assert!(
                !html.contains(marker),
                "an unthemed response must not be wrapped, found {marker}: {html}"
            );
        }

        disable_plugin(app).await;
    });
}

/// A JSON endpoint is untouched by any of this: same content type, same body.
#[test]
fn a_json_plugin_endpoint_is_unchanged_by_the_theme_seam() {
    common::run_test(async {
        let app = app();

        let cookies = app
            .create_and_login_user(
                "k1apitheme",
                "correct-horse-battery-staple",
                "k1t@test.local",
            )
            .await;

        let response = app
            .request_with_cookies(
                Request::get("/tpa/notes").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
        );
        let body = text_body(response).await;
        for marker in THEME_CHROME {
            assert!(!body.contains(marker), "a JSON body must not be themed");
        }

        disable_plugin(app).await;
    });
}

/// The token the plugin receives is bound to the caller's session, so one
/// visitor's form cannot be submitted from another visitor's session.
#[test]
fn a_token_minted_for_one_session_does_not_verify_in_another() {
    common::run_test(async {
        let app = app();

        let first = app
            .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
            .await;
        let token = token_from_form(&text_body(first).await);

        let second = app
            .request(Request::get("/tpa/form").body(Body::empty()).unwrap())
            .await;
        let other_cookies = common::extract_cookies(&second);

        let response = app
            .request_with_cookies(
                Request::post("/tpa/form")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("_token={token}&message=x")))
                    .unwrap(),
                &other_cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a token is bound to the session it was minted for"
        );

        disable_plugin(app).await;
    });
}

/// `/api/v1/openapi.json` describes the routes the server really serves.
///
/// It was a hand-written list built per request: no plugin route ever entered
/// it, and three of its paths were served by no router at all. The kernel half
/// is now generated from the declarations the router is built from
/// (`api_v1::kernel_api`); a unit test there holds that no `/api` route is
/// registered any other way. This asks for the document from an app with
/// `test_plugin_api` enabled, checks the plugin's `api` entries are in it and
/// its page entry is not, and then requests every path the document lists,
/// with the method it lists. A kernel path must reach its route: answered
/// with the `x-api-version` header (or, for a deprecated alias, the
/// `Deprecation` header) that only a matched route is given, and not 405. A
/// plugin path must answer other than 404 or 405.
#[test]
fn openapi_lists_plugin_routes_and_every_listed_path_is_served() {
    common::run_test(async {
        let app = app();

        let response = app
            .request(
                Request::get("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let doc = json_body(response).await;
        let paths = doc["paths"].as_object().expect("paths object");

        // The plugin's api entries, under the OpenAPI spelling of their params,
        // with the method, title and permission the entry declares.
        let write = &paths["/tpa/note/{slug}"]["post"];
        assert_eq!(write["summary"], "Write a note", "{doc}");
        assert!(
            write["description"]
                .as_str()
                .unwrap_or("")
                .contains(PERM_WRITE),
            "the required permission is stated: {write}"
        );
        assert!(paths["/tpa/notes"].get("get").is_some());
        assert!(
            paths.get("/tpa/page").is_none(),
            "a page entry is not an api"
        );

        // Every kernel route under /api/v1, the old paths as deprecated aliases.
        for listed in ["/api/v1/items", "/api/v1/item/{id}", "/api/v1/categories"] {
            assert_eq!(
                paths[listed]["get"]["deprecated"],
                serde_json::Value::Null,
                "{listed}"
            );
            let alias = listed.replace("/api/v1/", "/api/");
            assert_eq!(paths[&alias]["get"]["deprecated"], true, "{alias}");
        }
        assert!(!paths.contains_key("/api/v1/items/{id}"), "never served");

        // A real item, so `/api/item/{id}` answers for the route, not for a
        // missing row.
        let item_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ('page', 'page', 'openapi probe', true, 'Title', 'core', '{}'::jsonb) \
             ON CONFLICT (type) DO NOTHING",
        )
        .execute(&app.db)
        .await
        .expect("seed the page type");
        sqlx::query(
            "INSERT INTO item (id, type, title, status, author_id, created, changed, stage_id) \
             SELECT $1, 'page', 'openapi probe', 1, id, 1, 1, $2 FROM users LIMIT 1",
        )
        .bind(item_id)
        .bind(trovato_kernel::models::stage::LIVE_STAGE_ID)
        .execute(&app.db)
        .await
        .expect("seed an item");

        let mut unanswered = Vec::new();
        for (path, operations) in paths {
            let concrete = path
                .replace("{id}", &item_id.to_string())
                .split('/')
                .map(|seg| {
                    if seg.starts_with('{') {
                        "probe".to_string()
                    } else {
                        seg.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            for (method, operation) in operations.as_object().expect("operations") {
                let request = Request::builder()
                    .method(method.to_uppercase().as_str())
                    .uri(&concrete)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap();
                let response = app.request(request).await;
                let status = response.status();
                let routed = if operation["deprecated"] == true {
                    response.headers().get("deprecation").is_some()
                } else if path.starts_with("/api/v1/") {
                    response.headers().get("x-api-version").is_some()
                } else {
                    status != StatusCode::NOT_FOUND
                };
                if !routed || status == StatusCode::METHOD_NOT_ALLOWED {
                    unanswered.push(format!("{method} {path} -> {status}"));
                }
            }
        }
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(item_id)
            .execute(&app.db)
            .await
            .ok();
        assert!(
            unanswered.is_empty(),
            "every path in /api/v1/openapi.json must be served: {unanswered:?}"
        );
        disable_plugin(app).await;
    });
}

/// POST a JSON body to `/tpa/filter` with a fresh token and read the answer.
async fn filter_through_plugin(
    app: &TestApp,
    cookies: &str,
    text: &str,
    format: &str,
) -> serde_json::Value {
    let token = csrf_token(app, cookies).await;
    let response = app
        .request_with_cookies(
            Request::post("/tpa/filter")
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", &token)
                .body(Body::from(
                    serde_json::json!({"text": text, "format": format}).to_string(),
                ))
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

/// **B71 Part 5.** A plugin rendering user HTML filters it through the
/// kernel's text format with `host::filter_text`, as the user it acts for:
/// `filtered_html` drops a `<script>`, and `full_html` without `use full_html`
/// is an error, not unfiltered output.
#[test]
fn a_plugin_filters_user_html_through_a_text_format() {
    common::run_test(async {
        let app = app();
        let cookies = app
            .create_and_login_user(
                "b71filterer",
                "correct-horse-battery-staple",
                "b71filterer@test.local",
            )
            .await;
        let user = user_id_of(app, "b71filterer").await;
        grant_permission(app, user, "use filtered_html").await;

        let answer = filter_through_plugin(
            app,
            &cookies,
            "<p>Hi</p><script>alert(1)</script>",
            "filtered_html",
        )
        .await;
        let html = answer["html"]
            .as_str()
            .unwrap_or_else(|| panic!("{answer}"));
        assert!(html.contains("<p>Hi</p>"), "{answer}");
        assert!(!html.contains("<script"), "the script survived: {answer}");

        let answer = filter_through_plugin(app, &cookies, "<b>x</b>", "full_html").await;
        assert_eq!(
            answer["code"],
            serde_json::json!(trovato_sdk::host_errors::ERR_TEXT_FORMAT_NOT_PERMITTED),
            "full_html without `use full_html` must be refused: {answer}"
        );

        disable_plugin(app).await;
    });
}

// =============================================================================
// B182 — a plugin route's multipart form, end to end (G-FILE-NO-HOST-API,
// BL-107).
//
// Before this, a plugin route's body was UTF-8 text capped at `MAX_REQUEST_BODY`
// for the trip across the WASM boundary, so a multipart upload was either over
// the cap or opaque bytes no plugin could store. The kernel now parses
// `multipart/form-data` for a plugin route itself, stores each file part
// through `FileService` — the same size limit, MIME allowlist and magic-byte
// check `/file/upload` gets — and hands the plugin `ApiRequest::files`: ids and
// metadata, never bytes. `/tpa/upload/echo` and `/tpa/upload` are the real
// `test_plugin_api` wasm exercising both halves: receiving the upload, and
// attaching its id to an item through `host::save_item`.
// =============================================================================

/// A small but real PNG, the shape `FileService::validate_file`'s magic-byte
/// check needs.
fn small_png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 30, y as u8 * 30, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// A `multipart/form-data` body: one file part under `file_field`, plus
/// ordinary text fields, with a fixed boundary for reproducibility.
fn multipart_body(
    boundary: &str,
    file_field: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
    text_fields: &[(&str, &str)],
) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in text_fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; \
             filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// POST a multipart upload with a fresh CSRF token, returning the status and
/// parsed JSON body.
async fn post_upload(
    app: &TestApp,
    cookies: &str,
    path: &str,
    token: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
    text_fields: &[(&str, &str)],
) -> (StatusCode, serde_json::Value) {
    let boundary = "----B182PluginUploadBoundary";
    let body = multipart_body(boundary, "file", filename, content_type, data, text_fields);
    let response = app
        .request_with_cookies(
            Request::post(path)
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("X-CSRF-Token", token)
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, json_body(response).await)
}

/// **The receive half.** A plugin route's multipart POST reaches the plugin
/// with a stored file's id and metadata, not bytes — and the file really is in
/// `FileService`, `Temporary`, until something references it. `/tpa/upload/echo`
/// never calls `save-item`, so this is the control for the next test: nothing
/// promotes a file just for having been uploaded.
#[test]
fn a_multipart_post_hands_the_plugin_a_stored_files_id_and_metadata() {
    common::run_test(async {
        let app = app();
        let cookies = app
            .create_and_login_admin(
                "b182upload1",
                "correct-horse-battery-staple",
                "b182u1@test.local",
            )
            .await;
        let token = csrf_token(app, &cookies).await;

        let png = small_png();
        let (status, body) = post_upload(
            app,
            &cookies,
            "/tpa/upload/echo",
            &token,
            "logo.png",
            "image/png",
            &png,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["field"], "file", "{body}");
        assert_eq!(body["filename"], "logo.png", "{body}");
        assert_eq!(body["mime_type"], "image/png", "{body}");
        assert_eq!(body["size"], png.len() as i64, "{body}");

        let file_id: Uuid = body["file_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no file_id in {body}"))
            .parse()
            .expect("file_id is a uuid");
        let stored = app
            .state
            .files()
            .get(file_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("the kernel must have stored the file"));
        assert_eq!(stored.filemime, "image/png");
        assert_eq!(stored.filesize, png.len() as i64);
        assert_eq!(
            stored.status,
            FileStatus::Temporary,
            "no save referenced it, so it must not be promoted"
        );

        disable_plugin(app).await;
    });
}

/// **The promote half.** The plugin attaches the file id to a `conference`
/// item's `field_logo` through `host::save_item`; the kernel's existing
/// item-write path promotes it out of `Temporary`, exactly as it would for a
/// person's own upload widget — the plugin never touches file storage itself.
#[test]
fn saving_an_item_through_the_plugin_promotes_the_uploaded_file() {
    common::run_test(async {
        let app = app();
        app.ensure_conference_type().await;
        let cookies = app
            .create_and_login_admin(
                "b182upload2",
                "correct-horse-battery-staple",
                "b182u2@test.local",
            )
            .await;
        let token = csrf_token(app, &cookies).await;

        let png = small_png();
        let (status, body) = post_upload(
            app,
            &cookies,
            "/tpa/upload",
            &token,
            "logo.png",
            "image/png",
            &png,
            &[("title", "Conference with a logo")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["item_id"].is_string(), "{body}");

        let file_id: Uuid = body["file_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no file_id in {body}"))
            .parse()
            .expect("file_id is a uuid");
        let stored = app.state.files().get(file_id).await.unwrap().unwrap();
        assert_eq!(
            stored.status,
            FileStatus::Permanent,
            "saving an item referencing the file must promote it"
        );

        let item_id: Uuid = body["item_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no item_id in {body}"))
            .parse()
            .expect("item_id is a uuid");
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(item_id)
            .execute(&app.db)
            .await
            .ok();
        disable_plugin(app).await;
    });
}

/// **The kernel's existing refusal, unchanged.** A file over `MAX_FILE_SIZE` is
/// a 413 naming the limit; a disallowed type is a 415 — `FileService`'s own
/// checks, reached through the plugin route exactly as `/file/upload` reaches
/// them. No second storage path means no second set of rules either.
#[test]
fn an_oversized_or_disallowed_upload_is_refused_by_the_kernels_own_checks() {
    common::run_test(async {
        let app = app();
        let cookies = app
            .create_and_login_admin(
                "b182upload3",
                "correct-horse-battery-staple",
                "b182u3@test.local",
            )
            .await;

        let mut huge = small_png();
        huge.resize(11 * 1024 * 1024, 0);
        let token = csrf_token(app, &cookies).await;
        let (status, body) = post_upload(
            app,
            &cookies,
            "/tpa/upload/echo",
            &token,
            "huge.png",
            "image/png",
            &huge,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");

        let token = csrf_token(app, &cookies).await;
        let (status, body) = post_upload(
            app,
            &cookies,
            "/tpa/upload/echo",
            &token,
            "virus.exe",
            "application/x-msdownload",
            b"not really an executable",
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");

        disable_plugin(app).await;
    });
}

/// A multipart POST with no `_token` field and no header is refused exactly as
/// a form-urlencoded one is — widening the body format must not open a forgery
/// hole.
#[test]
fn a_multipart_post_without_a_csrf_token_is_refused() {
    common::run_test(async {
        let app = app();
        let cookies = app
            .create_and_login_admin(
                "b182upload4",
                "correct-horse-battery-staple",
                "b182u4@test.local",
            )
            .await;

        let boundary = "----B182NoTokenBoundary";
        let body = multipart_body(boundary, "file", "x.png", "image/png", &small_png(), &[]);
        let response = app
            .request_with_cookies(
                Request::post("/tpa/upload/echo")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        disable_plugin(app).await;
    });
}

/// **The other half of the contract.** A non-multipart POST is untouched: same
/// JSON body handling, same CSRF rule, as every test above this section
/// already proves. This pins that a JSON caller's request carries no `files`
/// surprise.
#[test]
fn a_non_multipart_post_is_unchanged_by_the_multipart_seam() {
    common::run_test(async {
        let app = app();
        let cookies = app
            .create_and_login_user(
                "b182nonmp",
                "correct-horse-battery-staple",
                "b182nonmp@test.local",
            )
            .await;
        let user = user_id_of(app, "b182nonmp").await;
        grant_write_permission(app, user).await;
        cleanup(app, user).await;
        let token = csrf_token(app, &cookies).await;

        let response = app
            .request_with_cookies(
                Request::post("/tpa/note/nonmp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(r#"{"text":"still json"}"#))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["written"], true, "{body}");

        cleanup(app, user).await;
        disable_plugin(app).await;
    });
}
