#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The AI Assistant, driven through the real router (0.102).
//!
//! Everything here runs against the **real** `plugins/test_assistant_scope`
//! wasm, the real tap dispatcher and a real Postgres. The model is the one thing
//! that is not real: a scripted provider
//! ([`trovato_kernel::services::ai_tools::scripted_provider`]) stands in for it,
//! written straight into `site_config.ai_providers`, so a test can say "now the
//! model calls `read_widget`, then answers with text" and watch what the kernel
//! does about it.
//!
//! That the provider's `base_url` is a loopback address is deliberate and is the
//! seam these tests use: the admin form refuses a loopback URL as SSRF
//! prevention, and the chat paths do not re-validate at call time, so writing
//! the config row directly reaches the same code a real provider would.
//!
//! `test_assistant_scope` is `default_enabled = false`, so this file installs it
//! and builds its **own** `TestApp`: `AppState` resolves the enabled plugin set,
//! and therefore the assistant scope registry, at construction.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_assistant_scope/`.

mod common;

use common::TestApp;

const PLUGIN: &str = "test_assistant_scope";
const SCOPE: &str = "test_widget";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("assistant fixture app init thread panicked")
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
    // default `./plugins` resolves to nothing.
    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

// =============================================================================
// The registry, built at boot from the real plugin
// =============================================================================

#[test]
fn the_valid_scope_is_registered_and_carries_its_tools() {
    common::run_test(async {
        let app = app();
        let registry = app.state.assistant_scopes();

        let registered = registry
            .get(SCOPE)
            .expect("the fixture's valid scope must be registered");
        assert_eq!(registered.plugin, PLUGIN);
        assert_eq!(registered.scope.label, "Test widget");
        assert_eq!(registered.scope.permission, "configure test widget");
        assert_eq!(registered.scope.suggestions.len(), 2);
        assert!(registered.tool("read_widget").is_some());
        assert!(registered.tool("set_widget_color").is_some());
        assert!(registered.tool("fail_loudly").is_some());
        assert_eq!(registered.write_tool_count(), 1);
    });
}

#[test]
fn the_invalid_scope_is_dropped_and_the_rejection_names_the_reason() {
    common::run_test(async {
        let app = app();
        let registry = app.state.assistant_scopes();

        // One bad declaration must not take the good one with it, and must not
        // stop the site booting — which it demonstrably did not, since this app
        // exists.
        assert!(
            registry.get("broken_widget").is_none(),
            "a scope with a tool name containing a space must be dropped"
        );

        let rejection = registry
            .rejections()
            .iter()
            .find(|r| r.scope == "broken_widget")
            .expect("the drop must be recorded, not silent");
        assert_eq!(rejection.plugin, PLUGIN);
        assert!(
            rejection.reason.contains("read widget"),
            "the reason must name the offending tool: {}",
            rejection.reason
        );
    });
}

// =============================================================================
// Fixtures
// =============================================================================

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::services::ai_assistant::AssistantConfig;
use trovato_kernel::services::ai_tools::scripted_provider::{Recorder, Scripted, start};
use uuid::Uuid;

/// The scope permission the fixture declares.
const PERM: &str = "configure test widget";
/// The item-kind scope the fixture declares beside the string-kind one.
const ITEM_SCOPE: &str = "test_conference";

/// One site config, one assistant config, one `ai_defaults` row: every test in
/// this file writes all three, so they take turns.
///
/// A per-test provider port cannot be reconciled with a single site-wide
/// `ai_defaults` key any other way, and a test that read another test's provider
/// would consume its scripted responses.
static SITE_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Point the site at a fresh scripted provider and return its recorder.
///
/// The `base_url` is a loopback address, which the admin form refuses as SSRF
/// prevention and the chat paths do not re-validate at call time. Writing the
/// config row directly is therefore the seam that lets a test drive the real
/// outbound path.
async fn use_provider(app: &TestApp, protocol: &str) -> Recorder {
    let (base_url, recorder) = start().await;
    let providers = serde_json::json!([{
        "id": "scripted",
        "label": "Scripted",
        "protocol": protocol,
        "base_url": base_url,
        "api_key_env": "",
        "models": [{"operation": "chat", "model": "test-model"}],
        "rate_limit_rpm": 0,
        "enabled": true,
    }]);
    SiteConfig::set(&app.db, "ai_providers", providers)
        .await
        .expect("write ai_providers");
    SiteConfig::set(
        &app.db,
        "ai_defaults",
        serde_json::json!({"chat": "scripted"}),
    )
    .await
    .expect("write ai_defaults");
    recorder
}

/// The assistant configuration every test starts from: on, with generous limits.
fn base_config() -> AssistantConfig {
    AssistantConfig {
        enabled: true,
        rate_limit_per_hour: 0,
        ..AssistantConfig::default()
    }
}

async fn set_config(app: &TestApp, config: AssistantConfig) {
    app.state
        .ai_assistant()
        .save_config(&config)
        .await
        .expect("save assistant config");
}

/// A completion that only speaks.
fn says(text: &str) -> Scripted {
    Scripted::ok(serde_json::json!({
        "model": "test-model",
        "choices": [{
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
    }))
}

/// A completion that calls one tool.
fn calls(call_id: &str, tool: &str, arguments: &str) -> Scripted {
    Scripted::ok(serde_json::json!({
        "model": "test-model",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {"name": tool, "arguments": arguments}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25}
    }))
}

/// A completion that costs a lot, for the token-cap test.
fn says_expensively(text: &str, total: u32) -> Scripted {
    Scripted::ok(serde_json::json!({
        "model": "test-model",
        "choices": [{
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": total / 2, "completion_tokens": total / 2, "total_tokens": total}
    }))
}

/// The Anthropic shape of a text answer.
fn anthropic_says(text: &str) -> Scripted {
    Scripted::ok(serde_json::json!({
        "model": "test-model",
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 4}
    }))
}

/// The Anthropic shape of a tool call.
fn anthropic_calls(id: &str, tool: &str, input: serde_json::Value) -> Scripted {
    Scripted::ok(serde_json::json!({
        "model": "test-model",
        "content": [{"type": "tool_use", "id": id, "name": tool, "input": input}],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 20, "output_tokens": 5}
    }))
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8_000_000)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    serde_json::from_str(&body_string(response).await).unwrap_or(serde_json::Value::Null)
}

/// A fresh CSRF token for a session. Tokens are single use, so every
/// state-changing request in these tests takes a new one.
async fn csrf_token(app: &TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/user/login").body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    let html = body_string(response).await;
    for needle in ["name=\"csrf_token\"", "name=\"_token\"", "name=\"token\""] {
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
    panic!("no CSRF token in the login page");
}

async fn user_id_of(app: &TestApp, username: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .expect("find the test user")
}

/// Grant one permission to one user, through a role of one.
async fn grant(app: &TestApp, user: Uuid, permission: &str) {
    let role = format!("assistant_role_{user}");
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (id, name) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(&role)
    .fetch_one(&app.db)
    .await
    .expect("create the role");
    sqlx::query(
        "INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(role_id)
    .bind(permission)
    .execute(&app.db)
    .await
    .expect("grant the permission");
    sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(user)
        .bind(role_id)
        .execute(&app.db)
        .await
        .expect("assign the role");
    app.state.permissions().invalidate_user(user);
}

/// A per-run suffix for every scope id these tests open.
///
/// The database outlives the test process: without this, a second `cargo test`
/// would find the first run's open conversation still there and append to its
/// transcript, and every "the transcript now has exactly these entries"
/// assertion would fail on the second run and pass on the first. That is the
/// worst kind of test.
static RUN: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| Uuid::now_v7().simple().to_string());

/// The scope id for a widget in this run.
fn widget_id(name: &str) -> String {
    format!("{name}-{}", *RUN)
}

/// An administrator who also literally holds the scope's permission.
///
/// The kernel lets an administrator open any scope, but the plugin's own belt
/// calls `current_user_has_permission`, which checks the permission list
/// literally and has no `administer site` bypass. A test that expects a tool to
/// run therefore has to grant the permission for real — which is what a site
/// would do too.
async fn admin_who_may_configure(app: &TestApp, name: &str) -> String {
    let cookies = app
        .create_and_login_admin(name, "Password123!", &format!("{name}@test.local"))
        .await;
    let user = user_id_of(app, name).await;
    grant(app, user, PERM).await;
    cookies
}

/// Open the fixture's string-kind scope and return `(cookies, conversation_id)`.
async fn open_conversation(app: &TestApp, cookies: &str, widget: &str) -> Uuid {
    let widget = widget_id(widget);
    let response = app
        .request_with_cookies(
            Request::get(format!("/ai/assistant/{SCOPE}/{widget}"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "the page must render");
    let html = body_string(response).await;
    let marker = "data-conversation-id=\"";
    let pos = html.find(marker).unwrap_or_else(|| {
        panic!(
            "no conversation id in the page: {}",
            &html[..html.len().min(400)]
        )
    });
    let start = pos + marker.len();
    let end = html[start..]
        .find('"')
        .map(|p| start + p)
        .expect("closed attribute");
    Uuid::parse_str(&html[start..end]).expect("a uuid")
}

/// Send one message and return the raw SSE body.
async fn send_message(app: &TestApp, cookies: &str, conversation: Uuid, message: &str) -> String {
    let token = csrf_token(app, cookies).await;
    let response = app
        .request_with_cookies(
            Request::post(format!("/api/v1/assistant/{conversation}/message"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", token)
                .body(Body::from(
                    serde_json::json!({"message": message}).to_string(),
                ))
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "sending a message should stream, not fail"
    );
    body_string(response).await
}

/// The `type` of every SSE event in a body, in order.
fn event_types(sse: &str) -> Vec<String> {
    sse_events(sse)
        .into_iter()
        .filter_map(|value| value["type"].as_str().map(str::to_string))
        .collect()
}

/// Every SSE event in a body, parsed.
fn sse_events(sse: &str) -> Vec<serde_json::Value> {
    sse.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|payload| serde_json::from_str(payload.trim()).ok())
        .collect()
}

/// The transcript entries of a conversation, straight from the row.
async fn transcript(app: &TestApp, conversation: Uuid) -> Vec<serde_json::Value> {
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT transcript FROM ai_conversation WHERE id = $1")
            .bind(conversation)
            .fetch_one(&app.db)
            .await
            .expect("load the transcript");
    value.as_array().cloned().unwrap_or_default()
}

/// The `kind` of every transcript entry, in order.
async fn transcript_kinds(app: &TestApp, conversation: Uuid) -> Vec<String> {
    transcript(app, conversation)
        .await
        .into_iter()
        .filter_map(|entry| entry["kind"].as_str().map(str::to_string))
        .collect()
}

/// The key the `variables` host interface namespaces the widget's colour under.
const COLOR_KEY: &str = "plugin.test_assistant_scope.color";

/// The fixture plugin's widget colour, which its write tool sets.
async fn widget_color(app: &TestApp) -> Option<String> {
    SiteConfig::get(&app.db, COLOR_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// Clear the widget colour so a test starts from a known state.
async fn clear_widget_color(app: &TestApp) {
    sqlx::query("DELETE FROM site_config WHERE key = $1")
        .bind(COLOR_KEY)
        .execute(&app.db)
        .await
        .ok();
}

// =============================================================================
// The page
// =============================================================================

#[test]
fn an_anonymous_visitor_is_sent_to_the_login_form() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;

        let response = app
            .request(
                Request::get(format!("/ai/assistant/{SCOPE}/w1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/user/login"),
            "the bare login form, with no destination parameter to read"
        );
    });
}

#[test]
fn a_user_without_the_permission_is_refused_and_an_admin_is_not() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;

        let reader = app
            .create_and_login_user("asst_reader", "Password123!", "asst_reader@test.local")
            .await;
        let response = app
            .request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/w1"))
                    .body(Body::empty())
                    .unwrap(),
                &reader,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "use ai, use ai assistant and the scope's own permission are all required"
        );

        let admin = app
            .create_and_login_admin(
                "asst_admin_page",
                "Password123!",
                "asst_admin_page@test.local",
            )
            .await;
        let response = app
            .request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/w1"))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_string(response).await;
        assert!(
            html.contains("Widget w1"),
            "the plugin's title: {html:.400}"
        );
        assert!(
            html.contains("What colour is the widget?"),
            "the scope's suggestions must be offered"
        );
        assert!(html.contains("data-conversation-id="));
    });
}

#[test]
fn a_reader_granted_all_three_permissions_gets_in() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;

        let cookies = app
            .create_and_login_user(
                "asst_permitted",
                "Password123!",
                "asst_permitted@test.local",
            )
            .await;
        let user = user_id_of(app, "asst_permitted").await;
        for permission in ["use ai", "use ai assistant", PERM] {
            grant(app, user, permission).await;
        }

        let response = app
            .request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/w-permitted"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    });
}

#[test]
fn an_unknown_scope_a_stray_id_and_a_disabled_assistant_are_all_404() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let admin = app
            .create_and_login_admin("asst_404", "Password123!", "asst_404@test.local")
            .await;

        let response = app
            .request_with_cookies(
                Request::get("/ai/assistant/no_such_scope")
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // The invalid scope was dropped at boot, so its URL is a 404 too.
        let response = app
            .request_with_cookies(
                Request::get("/ai/assistant/broken_widget/x")
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // An id on an item-kind scope that names no real item.
        let response = app
            .request_with_cookies(
                Request::get(format!("/ai/assistant/{ITEM_SCOPE}/not-a-uuid"))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // The assistant switched off serves nothing, so a site that turns it off
        // leaves no live conversation URLs behind.
        set_config(
            app,
            AssistantConfig {
                enabled: false,
                ..base_config()
            },
        )
        .await;
        let response = app
            .request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/w1"))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a disabled assistant is a 404, pinned here so it cannot drift to 503"
        );
    });
}

#[test]
fn a_scope_id_on_a_site_wide_scope_is_a_404() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let admin = app
            .create_and_login_admin("asst_noneid", "Password123!", "asst_noneid@test.local")
            .await;

        // The fixture's scopes are String- and Item-kind, so this asserts the
        // rule through the registry rather than through a None-kind fixture: a
        // String scope refuses an empty id, and an Item scope refuses a
        // non-uuid, which is the same validate_scope_id path.
        let response = app
            .request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/{}", "x".repeat(200)))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "an over-long scope_id is not something this scope configures"
        );
    });
}

#[test]
fn opening_the_same_scope_twice_returns_the_same_conversation() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let admin = app
            .create_and_login_admin("asst_twice", "Password123!", "asst_twice@test.local")
            .await;

        let first = open_conversation(app, &admin, "w-twice").await;
        let second = open_conversation(app, &admin, "w-twice").await;
        assert_eq!(
            first, second,
            "reopening must find the conversation, not start another"
        );

        // And the partial unique index is what makes that true rather than the
        // lookup happening to run first.
        let user = user_id_of(app, "asst_twice").await;
        let open_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ai_conversation \
             WHERE user_id = $1 AND scope = $2 AND scope_id = $3 AND status = 'open'",
        )
        .bind(user)
        .bind(SCOPE)
        .bind(widget_id("w-twice"))
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(open_rows, 1);

        let duplicate = sqlx::query(
            "INSERT INTO ai_conversation \
             (id, user_id, plugin, scope, scope_id, title, status, snapshot, created, changed) \
             VALUES ($1, $2, 'p', $3, $4, 't', 'open', '', 0, 0)",
        )
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(SCOPE)
        .bind(widget_id("w-twice"))
        .execute(&app.db)
        .await;
        assert!(
            duplicate.is_err(),
            "ai_conversation_open_uq must refuse a second open conversation"
        );
    });
}

// =============================================================================
// Sending a message
// =============================================================================

#[test]
fn a_message_needs_a_csrf_header_and_a_sane_length() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        use_provider(app, "open_ai_compatible").await;
        let admin = app
            .create_and_login_admin("asst_guards", "Password123!", "asst_guards@test.local")
            .await;
        let conversation = open_conversation(app, &admin, "w-guards").await;

        let response = app
            .request_with_cookies(
                Request::post(format!("/api/v1/assistant/{conversation}/message"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "no CSRF header");

        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/v1/assistant/{conversation}/message"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", token)
                    .body(Body::from(
                        serde_json::json!({"message": "x".repeat(5000)}).to_string(),
                    ))
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "over 4096 chars"
        );
    });
}

#[test]
fn a_read_only_conversation_refuses_a_message() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let admin = app
            .create_and_login_admin("asst_ro", "Password123!", "asst_ro@test.local")
            .await;
        let conversation = open_conversation(app, &admin, "w-readonly").await;

        // Age it past the site's lifetime rather than closing it: a closed
        // conversation would 404 through a different branch.
        sqlx::query("UPDATE ai_conversation SET created = $2 WHERE id = $1")
            .bind(conversation)
            .bind(0_i64)
            .execute(&app.db)
            .await
            .unwrap();

        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/v1/assistant/{conversation}/message"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", token)
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    });
}

#[test]
fn a_text_only_turn_lands_in_the_transcript_and_the_usage_log() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(says("The widget is unset."));

        let admin = admin_who_may_configure(app, "asst_text").await;
        let user = user_id_of(app, "asst_text").await;
        let before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ai_usage_log WHERE user_id = $1 AND plugin_name = 'kernel_assistant'",
        )
        .bind(user)
        .fetch_one(&app.db)
        .await
        .unwrap();

        let conversation = open_conversation(app, &admin, "w-text").await;
        let sse = send_message(app, &admin, conversation, "what colour is it?").await;

        assert_eq!(
            event_types(&sse),
            vec!["turn_start", "assistant", "done"],
            "the stream says start, answer, done, in that order: {sse}"
        );
        assert_eq!(
            transcript_kinds(app, conversation).await,
            vec!["user", "assistant"]
        );

        let after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ai_usage_log WHERE user_id = $1 AND plugin_name = 'kernel_assistant'",
        )
        .bind(user)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(after, before + 1, "one model call, one usage row");

        // The system message carried the plugin's snapshot, which is the whole
        // reason tap_assistant_context exists.
        let sent = &recorder.requests()[0];
        let system = sent["messages"][0]["content"].as_str().unwrap_or_default();
        assert!(
            system.contains(&format!("Widget {} has color unset.", widget_id("w-text"))),
            "{system}"
        );
        assert!(system.contains("You configure a test widget."), "{system}");
    });
}

#[test]
fn a_read_tool_runs_and_its_result_reaches_the_next_request() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(calls("c1", "read_widget", "{}"));
        recorder.push(says("It is unset."));

        let admin = admin_who_may_configure(app, "asst_read").await;
        let conversation = open_conversation(app, &admin, "w-read").await;
        let sse = send_message(app, &admin, conversation, "what colour is it?").await;

        assert_eq!(
            event_types(&sse),
            vec![
                "turn_start",
                "tool_call",
                "tool_result",
                "assistant",
                "done"
            ],
            "{sse}"
        );
        assert_eq!(
            transcript_kinds(app, conversation).await,
            vec!["user", "tool_call", "tool_result", "assistant"]
        );

        // The second request carried the tool's answer back to the model.
        let requests = recorder.requests();
        assert_eq!(requests.len(), 2, "one call, one follow-up");
        let messages = requests[1]["messages"].as_array().unwrap();
        let tool_message = messages
            .iter()
            .find(|m| m["role"] == "tool")
            .expect("the tool result must be on the wire");
        assert!(
            tool_message["content"]
                .as_str()
                .unwrap_or_default()
                .contains("\"color\""),
            "{tool_message}"
        );
    });
}

#[test]
fn a_write_tool_proposes_and_changes_nothing() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(calls("c1", "set_widget_color", r#"{"color":"teal"}"#));
        recorder.push(says("I have proposed setting it to teal."));

        let admin = admin_who_may_configure(app, "asst_write").await;
        let conversation = open_conversation(app, &admin, "w-write").await;
        let sse = send_message(app, &admin, conversation, "make it teal").await;

        assert_eq!(
            event_types(&sse),
            vec!["turn_start", "proposal", "assistant", "done"],
            "{sse}"
        );

        let proposals: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT status, description, tool FROM ai_proposal WHERE conversation_id = $1",
        )
        .bind(conversation)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].0, "proposed");
        assert_eq!(
            proposals[0].1, "Set widget color to teal",
            "the plugin's Describe summary is what the person reads"
        );

        // The whole safety posture, asserted: the model asked, and nothing moved.
        assert_eq!(widget_color(app).await, None);

        // The model was told it is waiting, so it does not ask again.
        let requests = recorder.requests();
        let messages = requests[1]["messages"].as_array().unwrap();
        let tool_message = messages.iter().find(|m| m["role"] == "tool").unwrap();
        let content = tool_message["content"].as_str().unwrap_or_default();
        assert!(content.contains("\"status\":\"proposed\""), "{content}");
        assert!(content.contains("Do not call this tool again"), "{content}");

        assert!(
            transcript_kinds(app, conversation)
                .await
                .contains(&"proposal".to_string())
        );
    });
}

#[test]
fn applying_a_proposal_is_what_makes_the_change() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(calls("c1", "set_widget_color", r#"{"color":"teal"}"#));
        recorder.push(says("Proposed."));

        let admin = admin_who_may_configure(app, "asst_apply").await;
        let conversation = open_conversation(app, &admin, "w-apply").await;
        send_message(app, &admin, conversation, "make it teal").await;

        let proposal: Uuid =
            sqlx::query_scalar("SELECT id FROM ai_proposal WHERE conversation_id = $1")
                .bind(conversation)
                .fetch_one(&app.db)
                .await
                .unwrap();

        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!(
                    "/api/v1/assistant/{conversation}/proposals/{proposal}/apply"
                ))
                .header("X-CSRF-Token", token)
                .body(Body::empty())
                .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["proposal"]["status"], "applied");

        assert_eq!(widget_color(app).await.as_deref(), Some("teal"));

        let entries = transcript(app, conversation).await;
        let note = entries
            .iter()
            .rev()
            .find(|entry| entry["kind"] == "note")
            .expect("an Applied note");
        assert!(
            note["text"]
                .as_str()
                .unwrap()
                .starts_with("Applied: Set widget color to teal."),
            "{note}"
        );

        // A second apply finds the row already moved and says so rather than
        // running the write twice.
        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!(
                    "/api/v1/assistant/{conversation}/proposals/{proposal}/apply"
                ))
                .header("X-CSRF-Token", token)
                .body(Body::empty())
                .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    });
}

#[test]
fn the_no_javascript_paths_redirect_and_discard_executes_nothing() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(calls("c1", "set_widget_color", r#"{"color":"crimson"}"#));
        recorder.push(says("Proposed."));

        let admin = admin_who_may_configure(app, "asst_form").await;
        let conversation = open_conversation(app, &admin, "w-form").await;
        send_message(app, &admin, conversation, "make it crimson").await;

        let proposal: Uuid =
            sqlx::query_scalar("SELECT id FROM ai_proposal WHERE conversation_id = $1")
                .bind(conversation)
                .fetch_one(&app.db)
                .await
                .unwrap();

        // The form path: a `_token` field, because a plain <form> cannot set a
        // header. Discard, so nothing is executed.
        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!(
                    "/api/v1/assistant/{conversation}/proposals/{proposal}/discard"
                ))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("_token={token}")))
                .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some(format!("/ai/assistant/{SCOPE}/{}", widget_id("w-form")).as_str())
        );

        let status: String = sqlx::query_scalar("SELECT status FROM ai_proposal WHERE id = $1")
            .bind(proposal)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(status, "discarded");
        assert_eq!(widget_color(app).await, None, "discard executes nothing");
    });
}

#[test]
fn somebody_elses_proposal_is_a_404() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(calls("c1", "set_widget_color", r#"{"color":"amber"}"#));
        recorder.push(says("Proposed."));

        let owner = admin_who_may_configure(app, "asst_owner").await;
        let conversation = open_conversation(app, &owner, "w-owner").await;
        send_message(app, &owner, conversation, "make it amber").await;
        let proposal: Uuid =
            sqlx::query_scalar("SELECT id FROM ai_proposal WHERE conversation_id = $1")
                .bind(conversation)
                .fetch_one(&app.db)
                .await
                .unwrap();

        // Another administrator, who has every permission the site grants and
        // still cannot touch somebody else's working notes.
        let stranger = app
            .create_and_login_admin("asst_stranger", "Password123!", "asst_stranger@test.local")
            .await;
        let token = csrf_token(app, &stranger).await;
        let response = app
            .request_with_cookies(
                Request::post(format!(
                    "/api/v1/assistant/{conversation}/proposals/{proposal}/apply"
                ))
                .header("X-CSRF-Token", token)
                .body(Body::empty())
                .unwrap(),
                &stranger,
            )
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(widget_color(app).await, None);
    });
}

#[test]
fn a_bad_call_becomes_a_failed_result_and_the_loop_carries_on() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        // Three ways to get it wrong, then an answer.
        recorder.push(calls("c1", "no_such_tool", "{}"));
        recorder.push(calls("c2", "set_widget_color", "{not json"));
        recorder.push(calls("c3", "fail_loudly", "{}"));
        recorder.push(says("Sorry about that."));

        let admin = admin_who_may_configure(app, "asst_bad").await;
        let conversation = open_conversation(app, &admin, "w-bad").await;
        let sse = send_message(app, &admin, conversation, "go").await;

        let results: Vec<serde_json::Value> = sse_events(&sse)
            .into_iter()
            .filter(|event| event["type"] == "tool_result")
            .collect();
        assert_eq!(results.len(), 3, "{sse}");
        for result in &results {
            assert_eq!(result["ok"], false, "{result}");
        }
        assert!(
            results[0]["summary"]
                .as_str()
                .unwrap()
                .contains("no such tool"),
            "{:?}",
            results[0]
        );
        assert!(
            results[1]["summary"].as_str().unwrap().contains("color"),
            "a malformed arguments string fails the required-key check: {:?}",
            results[1]
        );
        assert_eq!(
            results[2]["summary"].as_str().unwrap(),
            "as requested",
            "a tool that says no reaches the model as its own message"
        );

        // And the turn still finished with the model's answer.
        assert!(
            event_types(&sse).contains(&"assistant".to_string()),
            "{sse}"
        );
        assert_eq!(recorder.request_count(), 4);
    });
}

#[test]
fn the_tool_call_limit_stops_the_loop_and_says_so() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(
            app,
            AssistantConfig {
                max_tool_calls_per_message: 2,
                ..base_config()
            },
        )
        .await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        // The model never stops asking.
        for i in 0..6 {
            recorder.push(calls(&format!("c{i}"), "read_widget", "{}"));
        }

        let admin = app
            .create_and_login_admin("asst_limit", "Password123!", "asst_limit@test.local")
            .await;
        let conversation = open_conversation(app, &admin, "w-limit").await;
        let sse = send_message(app, &admin, conversation, "keep looking").await;

        let executed = event_types(&sse)
            .iter()
            .filter(|kind| *kind == "tool_call")
            .count();
        assert_eq!(executed, 2, "exactly the configured number ran: {sse}");

        let note = sse_events(&sse)
            .into_iter()
            .find(|event| event["type"] == "note")
            .expect("the limit must be announced");
        assert!(
            note["text"].as_str().unwrap().contains("Tool call limit"),
            "{note}"
        );
    });
}

#[test]
fn history_bounding_drops_old_exchanges_and_says_it_did() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(
            app,
            AssistantConfig {
                max_history_exchanges: 1,
                ..base_config()
            },
        )
        .await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        for answer in ["one", "two", "three"] {
            recorder.push(says(answer));
        }

        let admin = app
            .create_and_login_admin("asst_hist", "Password123!", "asst_hist@test.local")
            .await;
        let conversation = open_conversation(app, &admin, "w-hist").await;
        for message in ["first", "second", "third"] {
            send_message(app, &admin, conversation, message).await;
        }

        let requests = recorder.requests();
        let messages = requests[2]["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(
            messages[1]["content"].as_str().unwrap(),
            "[Trovato] Earlier parts of this conversation were dropped to save space. \
             The context block above is current."
        );
        // System, the notice, and the one surviving exchange plus the new message.
        let texts: Vec<&str> = messages
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect();
        assert!(texts.contains(&"third"), "{texts:?}");
        assert!(
            !texts.contains(&"first"),
            "the oldest exchange is gone: {texts:?}"
        );
    });
}

#[test]
fn starting_over_closes_the_old_conversation_and_takes_a_fresh_snapshot() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(calls("c1", "set_widget_color", r#"{"color":"olive"}"#));
        recorder.push(says("Proposed."));

        let admin = admin_who_may_configure(app, "asst_reset").await;
        let conversation = open_conversation(app, &admin, "w-reset").await;
        send_message(app, &admin, conversation, "make it olive").await;

        // Change the world between, so a copied snapshot would be visibly wrong.
        SiteConfig::set(&app.db, COLOR_KEY, serde_json::json!("chartreuse"))
            .await
            .unwrap();

        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/v1/assistant/{conversation}/reset"))
                    .header("X-CSRF-Token", token)
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let fresh = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
        assert_ne!(fresh, conversation);
        assert!(
            body["snapshot"].as_str().unwrap().contains("chartreuse"),
            "the new conversation must see the world as it is now: {body}"
        );

        let old_status: String =
            sqlx::query_scalar("SELECT status FROM ai_conversation WHERE id = $1")
                .bind(conversation)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(old_status, "closed");

        // A proposal whose conversation is gone can never be applied, so it must
        // not be left saying it is waiting.
        let proposal_status: String =
            sqlx::query_scalar("SELECT status FROM ai_proposal WHERE conversation_id = $1")
                .bind(conversation)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(proposal_status, "discarded");
    });
}

#[test]
fn the_message_and_token_caps_each_make_the_page_read_only() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;

        // One message allowed, so the second open is already read-only.
        set_config(
            app,
            AssistantConfig {
                max_messages: 4,
                ..base_config()
            },
        )
        .await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(says("ok"));
        recorder.push(says_expensively("costly", 100_000));

        let admin = app
            .create_and_login_admin("asst_caps", "Password123!", "asst_caps@test.local")
            .await;
        let conversation = open_conversation(app, &admin, "w-caps").await;
        send_message(app, &admin, conversation, "one").await;

        // Four is the floor the clamp allows, so drive the count there directly
        // rather than sending four messages through a scripted provider.
        sqlx::query("UPDATE ai_conversation SET message_count = 4 WHERE id = $1")
            .bind(conversation)
            .execute(&app.db)
            .await
            .unwrap();
        let html = body_string(
            app.request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/{}", widget_id("w-caps")))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await,
        )
        .await;
        assert!(html.contains("message limit"), "{html:.600}");
        assert!(
            !html.contains("id=\"assistant-composer\""),
            "no composer when read-only"
        );

        // The token cap, separately: a scripted large usage is what spends it.
        sqlx::query("UPDATE ai_conversation SET message_count = 1, tokens_used = 0 WHERE id = $1")
            .bind(conversation)
            .execute(&app.db)
            .await
            .unwrap();
        set_config(
            app,
            AssistantConfig {
                max_tokens_per_conversation: 1_000,
                ..base_config()
            },
        )
        .await;
        let sse = send_message(app, &admin, conversation, "spend it all").await;
        assert!(sse.contains("token limit"), "{sse}");

        let html = body_string(
            app.request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/{}", widget_id("w-caps")))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await,
        )
        .await;
        assert!(html.contains("token limit"), "{html:.600}");
    });
}

#[test]
fn a_denying_budget_refuses_the_turn_before_the_provider_is_called() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(says("this must never be reached"));

        let admin = app
            .create_and_login_admin("asst_budget", "Password123!", "asst_budget@test.local")
            .await;
        let user = user_id_of(app, "asst_budget").await;
        let conversation = open_conversation(app, &admin, "w-budget").await;

        // A one-token limit on the role this user is alone in, so the budget
        // being spent is unambiguously theirs and no sibling test is touched.
        grant(app, user, PERM).await;
        let role = format!("assistant_role_{user}");
        SiteConfig::set(
            &app.db,
            "ai_token_budgets",
            serde_json::json!({
                "period": "monthly",
                "action_on_limit": "deny",
                "defaults": {"scripted": {role: 1}},
            }),
        )
        .await
        .unwrap();
        // Spend the allowance, so the check is against real usage.
        sqlx::query(
            "INSERT INTO ai_usage_log \
             (id, user_id, plugin_name, provider_id, operation, model, prompt_tokens, \
              completion_tokens, total_tokens, latency_ms, created) \
             VALUES ($1, $2, 'kernel_assistant', 'scripted', 'Chat', 'test-model', 50, 50, 100, 1, $3)",
        )
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(chrono::Utc::now().timestamp())
        .execute(&app.db)
        .await
        .unwrap();

        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/v1/assistant/{conversation}/message"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", token)
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
                &admin,
            )
            .await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = body_string(response).await;
        assert!(body.contains("token_budget"), "{body}");
        assert_eq!(
            recorder.request_count(),
            0,
            "a denied budget must not reach the provider"
        );

        sqlx::query("DELETE FROM site_config WHERE key = 'ai_token_budgets'")
            .execute(&app.db)
            .await
            .ok();
    });
}

#[test]
fn the_rate_limit_refuses_a_second_message_in_the_hour() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        trovato_kernel::routes::assistant::clear_assistant_rate_limits();
        set_config(
            app,
            AssistantConfig {
                rate_limit_per_hour: 1,
                ..base_config()
            },
        )
        .await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(says("ok"));

        let admin = app
            .create_and_login_admin("asst_rate", "Password123!", "asst_rate@test.local")
            .await;
        let conversation = open_conversation(app, &admin, "w-rate").await;
        send_message(app, &admin, conversation, "one").await;

        let token = csrf_token(app, &admin).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/v1/assistant/{conversation}/message"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", token)
                    .body(Body::from(r#"{"message":"two"}"#))
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        trovato_kernel::routes::assistant::clear_assistant_rate_limits();
    });
}

#[test]
fn the_conversation_reads_back_as_json_for_its_owner_only() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let recorder = use_provider(app, "open_ai_compatible").await;
        recorder.push(says("hello"));

        let owner = app
            .create_and_login_admin("asst_json", "Password123!", "asst_json@test.local")
            .await;
        let conversation = open_conversation(app, &owner, "w-json").await;
        send_message(app, &owner, conversation, "hi").await;

        let body = body_json(
            app.request_with_cookies(
                Request::get(format!("/api/v1/assistant/{conversation}"))
                    .body(Body::empty())
                    .unwrap(),
                &owner,
            )
            .await,
        )
        .await;
        assert_eq!(body["scope"], SCOPE);
        assert_eq!(body["transcript"].as_array().unwrap().len(), 2);
        assert_eq!(body["read_only"], false);
        assert!(body["limits"]["max_messages"].is_number());

        let stranger = app
            .create_and_login_admin("asst_json2", "Password123!", "asst_json2@test.local")
            .await;
        let response = app
            .request_with_cookies(
                Request::get(format!("/api/v1/assistant/{conversation}"))
                    .body(Body::empty())
                    .unwrap(),
                &stranger,
            )
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    });
}

// =============================================================================
// The Anthropic shape, end to end
// =============================================================================

#[test]
fn the_whole_happy_path_works_in_the_anthropic_shape_too() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        clear_widget_color(app).await;
        let recorder = use_provider(app, "anthropic").await;
        recorder.push(anthropic_calls("t1", "read_widget", serde_json::json!({})));
        recorder.push(anthropic_calls(
            "t2",
            "set_widget_color",
            serde_json::json!({"color": "indigo"}),
        ));
        recorder.push(anthropic_says("I have proposed indigo."));

        let admin = admin_who_may_configure(app, "asst_anthropic").await;
        let conversation = open_conversation(app, &admin, "w-anthropic").await;
        let sse = send_message(app, &admin, conversation, "make it indigo").await;

        assert_eq!(
            event_types(&sse),
            vec![
                "turn_start",
                "tool_call",
                "tool_result",
                "proposal",
                "assistant",
                "done"
            ],
            "{sse}"
        );

        // The Anthropic wire shape: a system field, tools with input_schema, and
        // a tool result answered in the very next user turn.
        let requests = recorder.requests();
        assert!(requests[0]["system"].is_string(), "{:?}", requests[0]);
        assert_eq!(requests[0]["tools"][0]["name"], "read_widget");
        assert!(requests[0]["tools"][0]["input_schema"].is_object());

        let messages = requests[1]["messages"].as_array().unwrap();
        let assistant_index = messages
            .iter()
            .position(|m| m["role"] == "assistant")
            .expect("the model's own turn is replayed");
        assert_eq!(messages[assistant_index]["content"][0]["type"], "tool_use");
        assert_eq!(messages[assistant_index + 1]["role"], "user");
        assert_eq!(
            messages[assistant_index + 1]["content"][0]["type"],
            "tool_result"
        );

        // And the proposal is a proposal, whatever the protocol.
        assert_eq!(widget_color(app).await, None);
        let status: String =
            sqlx::query_scalar("SELECT status FROM ai_proposal WHERE conversation_id = $1")
                .bind(conversation)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(status, "proposed");
    });
}

// =============================================================================
// The admin page and the item launcher
// =============================================================================

#[test]
fn the_admin_page_lists_scopes_and_saves_an_override() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        let admin = app
            .create_and_login_admin("asst_adminpg", "Password123!", "asst_adminpg@test.local")
            .await;

        let html = body_string(
            app.request_with_cookies(
                Request::get("/admin/system/ai-assistant")
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await,
        )
        .await;
        assert!(html.contains("test_widget"), "{html:.600}");
        assert!(html.contains("Test widget"));
        assert!(html.contains(PERM));
        // A dropped scope is visible here, not only in a startup log.
        assert!(html.contains("broken_widget"), "rejections must be shown");

        // Save an override and switch the scope off.
        let token = csrf_token(app, &admin).await;
        let form = [
            ("_token", token.as_str()),
            ("enabled", "1"),
            ("provider_id", ""),
            ("model", ""),
            ("temperature", "0.2"),
            ("turn_timeout_secs", "60"),
            ("max_tool_calls_per_message", "8"),
            ("max_messages", "40"),
            ("max_tokens_per_conversation", "60000"),
            ("max_history_exchanges", "12"),
            ("max_response_tokens", "1024"),
            ("snapshot_max_bytes", "12288"),
            ("tool_result_max_bytes", "16384"),
            ("rate_limit_per_hour", "0"),
            ("conversation_ttl_hours", "24"),
            ("core_prompt", "Custom core prompt."),
            ("scope_prompt[test_widget]", "Only say no."),
        ];
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let response = app
            .request_with_cookies(
                Request::post("/admin/system/ai-assistant")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let saved = app.state.ai_assistant().load_config().await.unwrap();
        assert_eq!(saved.core_prompt, "Custom core prompt.");
        let scope = saved.scopes.get(SCOPE).expect("the scope was saved");
        assert!(!scope.enabled, "an unchecked box switches the scope off");
        assert_eq!(scope.prompt_override.as_deref(), Some("Only say no."));

        // A disabled scope's page is gone.
        let response = app
            .request_with_cookies(
                Request::get(format!("/ai/assistant/{SCOPE}/{}", widget_id("w-admin")))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Reset restores the stock text.
        let token = csrf_token(app, &admin).await;
        let mut reset: Vec<(String, String)> = form
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        reset[0] = ("_token".to_string(), token);
        reset.push(("scope_enabled[test_widget]".to_string(), "1".to_string()));
        reset.push(("reset_core_prompt".to_string(), "1".to_string()));
        let body = reset
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        app.request_with_cookies(
            Request::post("/admin/system/ai-assistant")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
            &admin,
        )
        .await;

        let saved = app.state.ai_assistant().load_config().await.unwrap();
        assert_eq!(
            saved.core_prompt,
            trovato_kernel::services::ai_assistant::DEFAULT_CORE_PROMPT
        );
        assert!(saved.scopes.get(SCOPE).unwrap().enabled);

        set_config(app, base_config()).await;
    });
}

#[test]
fn the_launcher_appears_on_an_item_of_a_named_type_and_nowhere_else() {
    common::run_test(async {
        let app = app();
        let _guard = SITE_LOCK.lock().await;
        set_config(app, base_config()).await;
        app.ensure_conference_type().await;

        let admin = app
            .create_and_login_admin("asst_launch", "Password123!", "asst_launch@test.local")
            .await;
        let author = user_id_of(app, "asst_launch").await;

        let conference = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO item (id, type, title, fields, status, author_id, stage_id, created, changed) \
             VALUES ($1, 'conference', 'Assistant launcher conference', '{}'::jsonb, 1, $2, \
                     $3::uuid, $4, $4)",
        )
        .bind(conference)
        .bind(author)
        .bind(Uuid::parse_str(trovato_sdk::types::LIVE_STAGE_UUID).unwrap())
        .bind(chrono::Utc::now().timestamp())
        .execute(&app.db)
        .await
        .expect("seed a conference");

        let html = body_string(
            app.request_with_cookies(
                Request::get(format!("/item/{conference}"))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await,
        )
        .await;
        assert!(
            html.contains(&format!("/ai/assistant/{ITEM_SCOPE}/{conference}")),
            "the launcher must link to this item's conversation"
        );
        assert!(html.contains("assistant-launch"));

        // A type no scope names carries nothing.
        let page = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO item_type (type, label, description, plugin) \
             VALUES ('asst_other', 'Other', '', 'test_assistant_scope') \
             ON CONFLICT (type) DO NOTHING",
        )
        .execute(&app.db)
        .await
        .expect("seed an unrelated content type");
        sqlx::query(
            "INSERT INTO item (id, type, title, fields, status, author_id, stage_id, created, changed) \
             VALUES ($1, 'asst_other', 'Not configurable', '{}'::jsonb, 1, $2, $3::uuid, $4, $4)",
        )
        .bind(page)
        .bind(author)
        .bind(Uuid::parse_str(trovato_sdk::types::LIVE_STAGE_UUID).unwrap())
        .bind(chrono::Utc::now().timestamp())
        .execute(&app.db)
        .await
        .expect("seed an unrelated item");

        let html = body_string(
            app.request_with_cookies(
                Request::get(format!("/item/{page}"))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await,
        )
        .await;
        assert!(
            !html.contains("assistant-launch"),
            "a type no scope names must not advertise an assistant"
        );

        sqlx::query("DELETE FROM item WHERE id = ANY($1)")
            .bind(vec![conference, page])
            .execute(&app.db)
            .await
            .ok();
    });
}
