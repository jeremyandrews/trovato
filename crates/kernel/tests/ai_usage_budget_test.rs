#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Usage, budgets and limits on the one AI send path (B89, part two).
//!
//! The send path records every call it makes in `ai_usage_log`, with the
//! prompt and completion tokens the provider reported, and holds every call to
//! its caller's budget before it leaves: a person's token budget, the
//! site-level cap every anonymous visitor shares, a plugin's background caps.
//! These tests drive each AI path once against a local provider and read the
//! rows it wrote.
//!
//! The provider is a loopback stub that answers in the OpenAI-compatible shape,
//! streaming when asked to, and counts what reaches it; its exact URL is
//! allowed past the send path's loopback refusal.
//!
//! Requires Postgres + Redis (the shared `TestApp`); the plugin call also needs
//! the `test_ai_background` wasm built into `plugins/test_ai_background/`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, SiteConfig};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::search::SearchSettings;
use trovato_kernel::services::ai_provider::{AiCaller, allow_loopback_provider_for_tests};
use trovato_kernel::services::ai_token_budget::{BudgetConfig, UsageLogEntry};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};
use uuid::Uuid;

/// Site wide providers, budgets and settings: this file serializes its tests.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The provider id every row these tests write carries.
const PROVIDER: &str = "b89-usage";

/// What the stub reports for every chat call.
const PROMPT: i32 = 11;
const COMPLETION: i32 = 7;
const TOTAL: i32 = 18;

/// What it reports for an embedding.
const EMBED_TOKENS: i32 = 5;

/// The anonymous user's id, under which every anonymous call is recorded.
const ANONYMOUS: Uuid = Uuid::nil();

/// A loopback provider in the OpenAI-compatible shape, counting requests.
async fn stub() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let chat_hits = Arc::clone(&hits);
    let embed_hits = Arc::clone(&hits);
    let app = axum::Router::new()
        .route(
            "/chat/completions",
            post(move |body: String| {
                let hits = Arc::clone(&chat_hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                    let system = request["messages"][0]["content"].as_str().unwrap_or("");
                    // The classifier is answered with one of its labels; the
                    // expansion with a JSON list; everything else with prose.
                    let text = if system.contains("satisfied") {
                        "unmet"
                    } else if system.contains("JSON") {
                        r#"["one","two"]"#
                    } else {
                        "An answer."
                    };
                    if request["stream"] == true {
                        let sse = format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            serde_json::json!({"choices": [{"delta": {"content": text}}]}),
                            serde_json::json!({"choices": [], "usage": {
                                "prompt_tokens": PROMPT,
                                "completion_tokens": COMPLETION,
                                "total_tokens": TOTAL,
                            }}),
                        );
                        return ([(header::CONTENT_TYPE, "text/event-stream")], sse)
                            .into_response();
                    }
                    axum::Json(serde_json::json!({
                        "model": "stub-model",
                        "choices": [{"message": {"role": "assistant", "content": text},
                                     "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": PROMPT, "completion_tokens": COMPLETION,
                                  "total_tokens": TOTAL},
                    }))
                    .into_response()
                }
            }),
        )
        .route(
            "/embeddings",
            post(move || {
                let hits = Arc::clone(&embed_hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "model": "stub-embed",
                        "data": [{"index": 0, "embedding": [0.25, -0.5, 0.75]}],
                        "usage": {"prompt_tokens": EMBED_TOKENS, "total_tokens": EMBED_TOKENS},
                    }))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base_url = format!("http://{addr}");
    allow_loopback_provider_for_tests(&base_url);
    (base_url, hits)
}

/// The site wide keys these tests write.
const SITE_KEYS: [&str; 4] = ["ai_providers", "ai_defaults", "search", "ai_token_budgets"];

/// Point Chat and Embedding at the stub, with the search settings and budget
/// given, and return a guard that puts every key back.
async fn configure(
    app: &'static TestApp,
    base_url: &str,
    settings: SearchSettings,
    budget: Option<BudgetConfig>,
) -> Restore {
    let mut saved = Vec::new();
    for key in SITE_KEYS {
        saved.push((key, SiteConfig::get(&app.db, key).await.unwrap()));
    }
    SiteConfig::set(
        &app.db,
        "ai_providers",
        serde_json::json!([{
            "id": PROVIDER,
            "label": "Usage stub",
            "protocol": "open_ai_compatible",
            "base_url": base_url,
            "api_key_env": "",
            "models": [
                {"operation": "chat", "model": "stub-model"},
                {"operation": "embedding", "model": "stub-embed"},
            ],
            "rate_limit_rpm": 0,
            "enabled": true,
        }]),
    )
    .await
    .unwrap();
    SiteConfig::set(
        &app.db,
        "ai_defaults",
        serde_json::json!({"chat": PROVIDER, "embedding": PROVIDER}),
    )
    .await
    .unwrap();
    settings.save(&app.db, app.state.cache()).await.unwrap();
    match budget {
        Some(config) => {
            app.state.ai_budgets().save_config(&config).await.unwrap();
        }
        None => {
            let _ = SiteConfig::delete(&app.db, "ai_token_budgets").await;
        }
    }
    Restore { app, saved }
}

/// Puts the site wide keys back when dropped... asynchronously, by `restore`.
struct Restore {
    app: &'static TestApp,
    saved: Vec<(&'static str, Option<serde_json::Value>)>,
}

impl Restore {
    async fn restore(self) {
        for (key, value) in self.saved {
            match value {
                Some(v) => SiteConfig::set(&self.app.db, key, v).await.unwrap(),
                None => {
                    let _ = SiteConfig::delete(&self.app.db, key).await;
                }
            }
        }
        self.app.state.cache().invalidate("search_settings").await;
    }
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8_000_000)
        .await
        .unwrap();
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

/// POST JSON to `path`, signed in with `cookies` if there are any, with a CSRF
/// token when signed in. Returns the status and the body.
async fn post_json(
    app: &TestApp,
    path: &str,
    body: serde_json::Value,
    cookies: Option<&str>,
    ip: &str,
) -> (StatusCode, String) {
    let mut request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", ip);
    let response = match cookies {
        Some(cookies) => {
            request = request.header("x-csrf-token", csrf_token(app, cookies).await);
            app.request_with_cookies(request.body(Body::from(body.to_string())).unwrap(), cookies)
                .await
        }
        None => {
            app.request(request.body(Body::from(body.to_string())).unwrap())
                .await
        }
    };
    let status = response.status();
    (status, body_string(response).await)
}

/// The usage rows this provider has had since `since`, oldest first:
/// `(source, operation, user_id, prompt, completion, total, model)`.
type Row = (String, String, Option<Uuid>, i32, i32, i32, String);

async fn rows_since(app: &TestApp, since: i64) -> Vec<Row> {
    sqlx::query_as(
        "SELECT plugin_name, operation, user_id, prompt_tokens, completion_tokens, total_tokens, \
                model \
         FROM ai_usage_log WHERE provider_id = $1 AND created >= $2 ORDER BY created, id",
    )
    .bind(PROVIDER)
    .bind(since)
    .fetch_all(&app.db)
    .await
    .unwrap()
}

/// The rows from `source`, waiting up to five seconds for `count` of them
/// (the sentiment worker writes off the request path).
async fn rows_from(app: &TestApp, since: i64, source: &str, count: usize) -> Vec<Row> {
    for _ in 0..50 {
        let rows: Vec<Row> = rows_since(app, since)
            .await
            .into_iter()
            .filter(|r| r.0 == source)
            .collect();
        if rows.len() >= count {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "expected {count} usage rows from {source}; have {:?}",
        rows_since(app, since).await
    );
}

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// The first whole second, on the database clock that stamps `ai_usage_log.created`,
/// that no earlier row can carry. `created` is whole seconds, and the test that
/// ran just before this one in the `ai-send-path` group may have written rows to
/// the same provider in the second this one starts in (the site cap test seeds
/// a 60 token anonymous search row); waiting for the next second keeps them out
/// of `rows_since`.
async fn a_fresh_second(app: &TestApp) -> i64 {
    let db_now = || async {
        sqlx::query_scalar::<_, i64>("SELECT EXTRACT(EPOCH FROM clock_timestamp())::bigint")
            .fetch_one(&app.db)
            .await
            .unwrap()
    };
    let started = db_now().await;
    loop {
        let now = db_now().await;
        if now > started {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One call on each AI path writes one usage row, with the tokens the provider
/// reported, attributed to whoever the call was for. Run with `--nocapture` to
/// see the table.
#[test]
fn one_call_on_each_path_records_its_real_usage() {
    run_test(async {
        let app = shared_app().await;
        let _serial = SERIAL.lock().await;
        let (base_url, _hits) = stub().await;
        let since = a_fresh_second(app).await;
        let restore = configure(
            app,
            &base_url,
            SearchSettings {
                sentiment_enabled: true,
                ..SearchSettings::default()
            },
            None,
        )
        .await;

        // AI search: expansion and follow-up as an anonymous visitor, the
        // summary as a signed-in member.
        let query = format!("b89 usage {}", Uuid::now_v7().simple());
        let (status, body) = post_json(
            app,
            "/api/v1/search/expand",
            serde_json::json!({"query": query}),
            None,
            "10.89.0.1",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "expand: {body}");

        let member = common::username("b89_member");
        app.create_test_user(&member, "Password123!", &format!("{member}@test.local"))
            .await;
        let member_id = common::user_id_of(app, &member).await;
        let member_cookies = app.login(&member, "Password123!").await;

        // The summary is built server-side from a result URL now, not from
        // browser-supplied text: a real, published item for it to resolve.
        app.ensure_conference_type().await;
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let item_id = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: format!("b89 usage summary target {query}"),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_body": { "value": "Some results." }
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("b89 usage budget test".to_string()),
                },
                &admin,
            )
            .await
            .unwrap()
            .id;

        let (status, body) = post_json(
            app,
            "/api/v1/search/summarize",
            serde_json::json!({
                "query": query,
                "results": [{"url": format!("/item/{item_id}")}],
            }),
            Some(&member_cookies),
            &common::test_ip_for(&member),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "summarize: {body}");

        let (status, body) = post_json(
            app,
            "/api/v1/search/followup",
            serde_json::json!({"messages": [
                {"role": "user", "content": "Search query: rust"},
                {"role": "assistant", "content": "RustConf is in Portland."},
                {"role": "user", "content": "And when?"},
            ]}),
            None,
            "10.89.0.2",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "followup: {body}");

        // The writing assistant and the streaming chatbot, as an administrator.
        let admin = common::username("b89_admin");
        let admin_cookies = app
            .create_and_login_admin(&admin, "Password123!", &format!("{admin}@test.local"))
            .await;
        let admin_id = common::user_id_of(app, &admin).await;
        let ip = common::test_ip_for(&admin);
        let (status, body) = post_json(
            app,
            "/api/v1/ai/assist",
            serde_json::json!({"text": "Make this better.", "operation": "rewrite"}),
            Some(&admin_cookies),
            &ip,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "assist: {body}");
        let (status, body) = post_json(
            app,
            "/api/v1/chat",
            serde_json::json!({"message": "hello"}),
            Some(&admin_cookies),
            &ip,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "chat: {body}");
        assert!(body.contains("\"done\""), "the stream finished: {body}");

        // A plugin's background `ai-request`.
        let mut runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
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
        let result = dispatcher
            .dispatch_to_plugin(
                "tap_cron",
                "{}",
                "test_ai_background",
                RequestState::new(UserContext::background(), services),
            )
            .await
            .unwrap();
        assert!(result.output.contains("\"ok\""), "{}", result.output);

        // The kernel's own embedding of an item.
        let embedded = app
            .state
            .ai_providers()
            .embed("index me", &AiCaller::kernel("__kernel_embed", true))
            .await
            .unwrap()
            .expect("an embedding provider is configured");
        assert_eq!(embedded.vector.len(), 3);

        let sentiment = rows_from(app, since, "kernel_search_sentiment", 1).await;
        let rows = rows_since(app, since).await;
        restore.restore().await;

        println!("\nai_usage_log, provider {PROVIDER}, one call on each path:");
        println!(
            "{:<24} {:<10} {:<36} {:>6} {:>10} {:>6}  model",
            "source", "operation", "user_id", "prompt", "completion", "total"
        );
        for (source, op, user, prompt, completion, total, model) in &rows {
            println!(
                "{source:<24} {op:<10} {:<36} {prompt:>6} {completion:>10} {total:>6}  {model}",
                user.map_or("NULL".to_string(), |u| u.to_string())
            );
        }

        let chat = |source: &str, user: Option<Uuid>| {
            let row = rows
                .iter()
                .find(|r| r.0 == source && r.2 == user)
                .unwrap_or_else(|| panic!("no {source} row for {user:?}: {rows:?}"));
            assert_eq!(
                (row.1.as_str(), row.3, row.4, row.5),
                ("Chat", PROMPT, COMPLETION, TOTAL),
                "{source}: {row:?}"
            );
        };
        chat("kernel_search", Some(ANONYMOUS)); // expand and follow-up
        chat("kernel_search", Some(member_id)); // summary
        chat("trovato_ai", Some(admin_id)); // writing assistant
        chat("kernel_chat", Some(admin_id)); // streaming chatbot
        chat("test_ai_background", None); // plugin, background
        chat("kernel_search_sentiment", Some(ANONYMOUS)); // classifier
        assert_eq!(sentiment.len(), 1);
        assert_eq!(
            rows.iter()
                .filter(|r| r.0 == "kernel_search" && r.2 == Some(ANONYMOUS))
                .count(),
            2,
            "expansion and follow-up: {rows:?}"
        );
        let embedding = rows
            .iter()
            .find(|r| r.0 == "__kernel_embed")
            .expect("the embedding is recorded");
        assert_eq!(
            (
                embedding.1.as_str(),
                embedding.2,
                embedding.3,
                embedding.4,
                embedding.5
            ),
            ("Embedding", None, EMBED_TOKENS, 0, EMBED_TOKENS)
        );
    });
}

/// Seed a usage row, as if `user` had already spent `tokens` this period.
async fn spend(app: &TestApp, user: Option<Uuid>, source: &str, tokens: i32) {
    app.state
        .ai_budgets()
        .record_usage(
            &app.db,
            UsageLogEntry {
                user_id: user,
                plugin_name: source.to_string(),
                provider_id: PROVIDER.to_string(),
                operation: "Chat".to_string(),
                model: "stub-model".to_string(),
                prompt_tokens: tokens,
                completion_tokens: 0,
                total_tokens: tokens,
                latency_ms: 1,
                cost_estimate: None,
            },
        )
        .await
        .unwrap();
}

/// The writing assistant is held to the person's token budget: a member over
/// it gets a 429 and the provider is never called.
#[test]
fn the_writing_assistant_is_held_to_the_persons_budget() {
    run_test(async {
        let app = shared_app().await;
        let _serial = SERIAL.lock().await;
        let (base_url, hits) = stub().await;
        let restore = configure(
            app,
            &base_url,
            SearchSettings::default(),
            Some(BudgetConfig::default()),
        )
        .await;

        let (user_id, cookies) =
            common::user_holding(app, "b89_assist_budget", &["use ai", "use ai chat"]).await;
        app.state
            .ai_budgets()
            .set_user_override(&app.db, user_id, PROVIDER, 100)
            .await
            .unwrap();
        spend(app, Some(user_id), "trovato_ai", 150).await;

        let (status, body) = post_json(
            app,
            "/api/v1/ai/assist",
            serde_json::json!({"text": "Make this better.", "operation": "rewrite"}),
            Some(&cookies),
            &format!("10.89.1.{}", user_id.as_bytes()[15]),
        )
        .await;
        restore.restore().await;

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "the provider was never called"
        );
    });
}

/// Anonymous AI search shares one site-level cap: the `anonymous user` role's
/// limit, metered over every anonymous call. Past it, an anonymous expansion is
/// refused with a 429 before the provider is called, while a signed-in member,
/// held to their own budget, is still served.
#[test]
fn anonymous_search_is_held_to_a_site_level_cap() {
    run_test(async {
        let app = shared_app().await;
        let _serial = SERIAL.lock().await;
        let (base_url, hits) = stub().await;
        let mut budget = BudgetConfig::default();
        budget.defaults.insert(
            PROVIDER.to_string(),
            [("anonymous user".to_string(), 50u64)]
                .into_iter()
                .collect(),
        );
        let restore = configure(app, &base_url, SearchSettings::default(), Some(budget)).await;
        spend(app, Some(ANONYMOUS), "kernel_search", 60).await;

        let query = format!("b89 capped {}", Uuid::now_v7().simple());
        let (status, body) = post_json(
            app,
            "/api/v1/search/expand",
            serde_json::json!({"query": query}),
            None,
            "10.89.2.1",
        )
        .await;
        let anonymous_hits = hits.load(Ordering::SeqCst);

        let member = common::username("b89_capped_member");
        app.create_test_user(&member, "Password123!", &format!("{member}@test.local"))
            .await;
        let cookies = app.login(&member, "Password123!").await;
        let (member_status, member_body) = post_json(
            app,
            "/api/v1/search/expand",
            serde_json::json!({"query": query}),
            Some(&cookies),
            &common::test_ip_for(&member),
        )
        .await;
        restore.restore().await;

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(anonymous_hits, 0, "the provider was never called");
        assert_eq!(member_status, StatusCode::OK, "{member_body}");
    });
}

/// The AI search endpoints ask for `use ai`. A default install grants it to
/// the anonymous role, so search works as before; take it away and an
/// anonymous visitor is refused before anything is sent.
#[test]
fn ai_search_asks_for_use_ai() {
    run_test(async {
        let app = shared_app().await;
        let _serial = SERIAL.lock().await;
        let anonymous_role = trovato_kernel::models::role::well_known::ANONYMOUS_ROLE_ID;
        let granted: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM role_permissions \
             WHERE role_id = $1 AND permission = 'use ai')",
        )
        .bind(anonymous_role)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(
            granted,
            "a default install grants `use ai` to the anonymous role"
        );

        let (base_url, hits) = stub().await;
        let restore = configure(app, &base_url, SearchSettings::default(), None).await;
        sqlx::query("DELETE FROM role_permissions WHERE role_id = $1 AND permission = 'use ai'")
            .bind(anonymous_role)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.permissions().invalidate_all();

        let mut refused = Vec::new();
        for (path, body) in [
            ("/api/v1/search/expand", serde_json::json!({"query": "q"})),
            (
                "/api/v1/search/summarize",
                serde_json::json!({"query": "q", "results": []}),
            ),
            (
                "/api/v1/search/followup",
                serde_json::json!({"messages": [{"role": "user", "content": "q"}]}),
            ),
        ] {
            let (status, _) = post_json(app, path, body, None, "10.89.3.1").await;
            refused.push((path, status));
        }

        sqlx::query(
            "INSERT INTO role_permissions (role_id, permission) VALUES ($1, 'use ai') \
             ON CONFLICT DO NOTHING",
        )
        .bind(anonymous_role)
        .execute(&app.db)
        .await
        .unwrap();
        app.state.permissions().invalidate_all();
        restore.restore().await;

        for (path, status) in refused {
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing was sent");
    });
}

/// Every per-operation breaker the send path uses is on the
/// `trovato_circuit_breaker_ai` gauge, beside the connection test's.
#[test]
fn every_ai_breaker_is_on_the_gauge() {
    run_test(async {
        let app = shared_app().await;
        let response = app
            .request(Request::get("/metrics").body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let text = body_string(response).await;
        for operation in [
            "chat",
            "embedding",
            "image_generation",
            "speech_to_text",
            "text_to_speech",
            "moderation",
            "connection_test",
        ] {
            let series = format!("trovato_circuit_breaker_ai{{operation=\"{operation}\"}}");
            assert!(text.contains(&series), "{series} missing from /metrics");
        }
    });
}
