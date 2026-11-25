#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every AI call goes through one send path (B89).
//!
//! The send path checks the provider's base URL at send time, not only when the
//! admin form saved it: a `site_config` row written any other way (an import, a
//! config sync, a direct edit) must not let a request reach a private address.
//! Before B89 only the embedding call re-checked; the chat, tool and plugin
//! paths connected to whatever IP literal the row named, because an IP literal
//! never passes through the hardened client's resolver.
//!
//! The "private address" in these tests is a loopback stub that counts the
//! requests it receives: the refusal is proven by the count staying at zero.
//!
//! Requires Postgres + Redis (the shared `TestApp`); the plugin test also needs
//! the `test_ai_background` wasm built into `plugins/test_ai_background/`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::ai_provider::allow_loopback_provider_for_tests;
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

/// Site wide providers: this file serializes its own tests.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Text only the stub's error body carries, so a response that repeats it has
/// leaked the provider's own words.
const PROVIDER_SECRET: &str = "provider-internal-detail-7f3a";

/// A loopback provider that counts every request and answers each one with a
/// 500 whose body is [`PROVIDER_SECRET`].
async fn counting_stub() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let handler = move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{{\"error\":{{\"message\":\"{PROVIDER_SECRET}\"}}}}"),
            )
        }
    };
    let app = axum::Router::new()
        .route("/chat/completions", post(handler.clone()))
        .route("/messages", post(handler.clone()))
        .route("/embeddings", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

/// Point the Chat operation at `base_url`, written straight into
/// `site_config` the way an import would, past the admin form's check.
async fn point_chat_at(app: &TestApp, base_url: &str) {
    SiteConfig::set(
        &app.db,
        "ai_providers",
        serde_json::json!([{
            "id": "b89-send-path",
            "label": "Send path stub",
            "protocol": "open_ai_compatible",
            "base_url": base_url,
            "api_key_env": "",
            "models": [{"operation": "chat", "model": "stub-model"}],
            "rate_limit_rpm": 0,
            "enabled": true,
        }]),
    )
    .await
    .expect("write ai_providers");
    SiteConfig::set(
        &app.db,
        "ai_defaults",
        serde_json::json!({"chat": "b89-send-path"}),
    )
    .await
    .expect("write ai_defaults");
}

/// Remove the provider these tests wrote, so the next test target does not
/// inherit a provider pointing at a port nothing listens on.
async fn clear_providers(app: &TestApp) {
    for key in ["ai_providers", "ai_defaults"] {
        sqlx::query("DELETE FROM site_config WHERE key = $1")
            .bind(key)
            .execute(&app.db)
            .await
            .ok();
    }
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8_000_000)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// A fresh CSRF token for a session.
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

/// POST `/api/v1/chat` as a fresh administrator and return `(status, body)`.
async fn chat_as_admin(app: &TestApp, name: &str) -> (StatusCode, String) {
    let name = common::username(name);
    let cookies = app
        .create_and_login_admin(&name, "Password123!", &format!("{name}@test.local"))
        .await;
    let token = csrf_token(app, &cookies).await;
    let response = app
        .request_with_cookies(
            Request::post("/api/v1/chat")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-csrf-token", token)
                .header("x-forwarded-for", common::test_ip_for(&name))
                .body(Body::from(r#"{"message":"hello"}"#))
                .unwrap(),
            &cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

/// **B50, chat.** A loopback IP literal written into the provider row is
/// refused when the streaming chat sends, and the provider is never contacted.
#[test]
fn the_chat_path_refuses_a_private_ip_literal_base_url() {
    run_test(async {
        let app = shared_app().await;
        let _serial = SERIAL.lock().await;
        let (base_url, hits) = counting_stub().await;
        point_chat_at(app, &base_url).await;

        let (status, body) = chat_as_admin(app, "b89_chat_refused").await;
        clear_providers(app).await;

        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "the chat path must not connect to a private address: {status} {body}"
        );
        assert!(
            !body.contains(PROVIDER_SECRET),
            "nothing of the provider's reaches the browser: {body}"
        );
    });
}

/// **B55.** A streaming chat that fails to start shows a generic message: the
/// provider's own error text goes to the log, never to the browser.
#[test]
fn a_failed_stream_start_shows_a_generic_message() {
    run_test(async {
        let app = shared_app().await;
        let _serial = SERIAL.lock().await;
        let (base_url, hits) = counting_stub().await;
        // This one is let through, so the provider really answers 500.
        allow_loopback_provider_for_tests(&base_url);
        point_chat_at(app, &base_url).await;

        let (status, body) = chat_as_admin(app, "b89_chat_generic").await;
        clear_providers(app).await;

        assert_eq!(hits.load(Ordering::SeqCst), 1, "the provider was called");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(
            !body.contains(PROVIDER_SECRET),
            "the provider's error text must not reach the browser: {body}"
        );
        assert!(
            !body.contains("HTTP 500"),
            "nor the provider's status line: {body}"
        );
    });
}

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// **B50, plugin.** A plugin's `ai-request` is refused at send time when the
/// provider row names a private IP literal.
#[test]
fn the_plugin_path_refuses_a_private_ip_literal_base_url() {
    run_test(async {
        let app = shared_app().await;
        let _serial = SERIAL.lock().await;
        let (base_url, hits) = counting_stub().await;
        point_chat_at(app, &base_url).await;

        let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
        runtime
            .load_plugin(&plugins_dir().join("test_ai_background"))
            .expect("load test_ai_background (build its wasm first)");
        let runtime = Arc::new(runtime);
        let registry = Arc::new(TapRegistry::from_plugins(&runtime));
        let dispatcher = TapDispatcher::new(runtime, registry);

        let services = RequestServices::for_background(
            app.db.clone(),
            Some(app.state.ai_providers().clone()),
            Some(app.state.ai_budgets().clone()),
            reqwest::Client::new(),
        )
        .with_plugin_runtime(dispatcher.runtime().clone());
        let state = RequestState::new(UserContext::background(), services);
        let result = dispatcher
            .dispatch_to_plugin("tap_cron", "{}", "test_ai_background", state)
            .await
            .expect("fixture implements tap_cron");
        clear_providers(app).await;

        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "the plugin path must not connect to a private address: {}",
            result.output
        );
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(
            output.get("ai_code").is_some(),
            "the plugin gets an error code: {output}"
        );
    });
}
