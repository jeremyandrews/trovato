#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Follow-up sentiment, classified off the request path.
//!
//! After a follow-up answer has been built, and only with sentiment and
//! analytics switched on, the visitor's message and the answer before it go to
//! a bounded channel; one background worker asks the configured provider for a
//! single word (`satisfied`, `confused` or `unmet`) and counts it in the daily
//! analytics. The visitor never waits for it, and the text itself is stored
//! nowhere.
//!
//! The provider is a local stub: an Anthropic shaped `/messages` endpoint that
//! answers follow-ups at once and holds classifier calls until the test lets
//! them through.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use common::{TestApp, defer_cleanup, run_test, shared_app};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::search::SearchSettings;
use trovato_kernel::services::ai_provider::{
    AiDefaults, AiOperationType, AiProviderConfig, OperationModel, ProviderProtocol,
};
use uuid::Uuid;

/// Site wide settings and providers: this file serializes its own tests.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The classifier prompt names the three labels; nothing else the search
/// endpoints send does.
const CLASSIFIER_MARKER: &str = "satisfied";

/// A stub provider: the classifier's answer, a gate holding classifier calls,
/// and a count of them.
struct Stub {
    label: std::sync::Mutex<String>,
    gate: tokio::sync::Semaphore,
    classifier_calls: AtomicUsize,
}

async fn serve_stub(stub: Arc<Stub>) -> String {
    let app = axum::Router::new().route(
        "/messages",
        post(move |body: String| {
            let stub = Arc::clone(&stub);
            async move {
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let system = request["system"].as_str().unwrap_or_default();
                let text = if system.contains(CLASSIFIER_MARKER) {
                    stub.classifier_calls.fetch_add(1, Ordering::SeqCst);
                    // Held here until the test releases a permit.
                    stub.gate.acquire().await.unwrap().forget();
                    stub.label.lock().unwrap().clone()
                } else {
                    "Here is a little more about that.".to_string()
                };
                axum::Json(serde_json::json!({
                    "id": "msg_stub",
                    "type": "message",
                    "role": "assistant",
                    "model": "stub-model",
                    "stop_reason": "end_turn",
                    "content": [{ "type": "text", "text": text }],
                    "usage": { "input_tokens": 1, "output_tokens": 1 },
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
    // A loopback stand-in: the send path refuses loopback, so this one exact
    // URL is allowed through for the test.
    trovato_kernel::services::ai_provider::allow_loopback_provider_for_tests(&base_url);
    base_url
}

/// Point Chat at the stub, switch sentiment as asked, and put every site wide
/// key back when the test ends.
async fn setup(app: &'static TestApp, sentiment: bool) -> Arc<Stub> {
    let saved: Vec<(&str, Option<serde_json::Value>)> = {
        let mut saved = Vec::new();
        for key in ["ai_providers", "ai_defaults", "search"] {
            saved.push((key, SiteConfig::get(&app.db, key).await.unwrap()));
        }
        saved
    };
    defer_cleanup(move || async move {
        for (key, value) in saved {
            match value {
                Some(v) => SiteConfig::set(&app.db, key, v).await.unwrap(),
                None => {
                    let _ = SiteConfig::delete(&app.db, key).await;
                }
            }
        }
        app.state.cache().invalidate("search_settings").await;
    });

    let stub = Arc::new(Stub {
        label: std::sync::Mutex::new("unmet".to_string()),
        gate: tokio::sync::Semaphore::new(0),
        classifier_calls: AtomicUsize::new(0),
    });
    let base_url = serve_stub(Arc::clone(&stub)).await;

    let providers = app.state.ai_providers();
    providers
        .save_provider(AiProviderConfig {
            id: "sentiment-stub".to_string(),
            label: "Sentiment stub".to_string(),
            protocol: ProviderProtocol::Anthropic,
            base_url,
            api_key_env: "TROVATO_SENTIMENT_TEST_KEY_UNSET".to_string(),
            models: vec![OperationModel {
                operation: AiOperationType::Chat,
                model: "stub-model".to_string(),
            }],
            rate_limit_rpm: 0,
            enabled: true,
        })
        .await
        .unwrap();
    let mut defaults = AiDefaults::default();
    defaults
        .defaults
        .insert(AiOperationType::Chat, "sentiment-stub".to_string());
    providers.save_defaults(defaults).await.unwrap();

    SearchSettings {
        sentiment_enabled: sentiment,
        ..SearchSettings::default()
    }
    .save(&app.db, app.state.cache())
    .await
    .unwrap();

    stub
}

/// Today's sentiment counters, after flushing what is pending.
async fn labels_today(app: &TestApp) -> (i64, i64, i64) {
    app.state.search().analytics().flush().await.unwrap();
    sqlx::query_as(
        "SELECT COALESCE(SUM(satisfied), 0)::bigint, COALESCE(SUM(confused), 0)::bigint, \
                COALESCE(SUM(unmet), 0)::bigint \
         FROM search_daily WHERE day = (now() AT TIME ZONE 'UTC')::date",
    )
    .fetch_one(&app.db)
    .await
    .unwrap()
}

/// Wait, up to ten seconds, for today's counters to differ from `before`.
async fn labels_change_from(app: &TestApp, before: (i64, i64, i64)) -> (i64, i64, i64) {
    for _ in 0..100 {
        let now = labels_today(app).await;
        if now != before {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the sentiment counters never changed from {before:?}");
}

async fn followup(app: &TestApp, question: &str) -> (StatusCode, String) {
    let body = serde_json::json!({"messages": [
        {"role": "user", "content": "Search query: rust\n\nSearch result excerpts:\n..."},
        {"role": "assistant", "content": "RustConf is in Portland."},
        {"role": "user", "content": question},
    ]});
    let response = app
        .request(
            Request::post("/api/v1/search/followup")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// (a) The answer comes back while the classifier is still held, and the label
/// is counted once the worker gets its reply.
#[test]
fn the_followup_answers_before_the_classifier_and_the_label_is_counted() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SERIAL.lock().await);
        let stub = setup(app, true).await;
        let before = labels_today(app).await;

        let (status, body) = tokio::time::timeout(
            Duration::from_secs(10),
            followup(app, "That does not answer my question at all"),
        )
        .await
        .expect("the follow-up must not wait for the classifier");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("a little more"), "{body}");

        // The classifier call is made, and still held.
        for _ in 0..100 {
            if stub.classifier_calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(stub.classifier_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            labels_today(app).await,
            before,
            "nothing counted while held"
        );

        stub.gate.add_permits(1);
        let after = labels_change_from(app, before).await;
        assert_eq!(after, (before.0, before.1, before.2 + 1), "one unmet");
    });
}

/// (b) A reply that is not one of the three labels counts nothing.
#[test]
fn a_reply_outside_the_three_labels_counts_nothing() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SERIAL.lock().await);
        let stub = setup(app, true).await;
        stub.gate.add_permits(2);
        let before = labels_today(app).await;

        *stub.label.lock().unwrap() = "Probably fine, I think".to_string();
        let (status, _) = followup(app, "ok thanks").await;
        assert_eq!(status, StatusCode::OK);
        // Wait until the worker has taken that job to the stub.
        for _ in 0..100 {
            if stub.classifier_calls.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // A second, valid one behind it: the worker is one queue, so once this
        // one is counted the first has been fully handled.
        *stub.label.lock().unwrap() = "confused".to_string();
        let (status, _) = followup(app, "what do you mean by that?").await;
        assert_eq!(status, StatusCode::OK);

        let after = labels_change_from(app, before).await;
        assert_eq!(
            after,
            (before.0, before.1 + 1, before.2),
            "only the valid label is counted"
        );
    });
}

/// (c) With sentiment off, no classifier call is made.
#[test]
fn with_sentiment_off_no_classifier_call_is_made() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SERIAL.lock().await);
        let stub = setup(app, false).await;
        stub.gate.add_permits(1);
        *stub.label.lock().unwrap() = "satisfied".to_string();
        let before = labels_today(app).await;

        let (status, _) = followup(app, "great, thanks").await;
        assert_eq!(status, StatusCode::OK);

        // Then switch it on and ask again; once that one is counted, the queue
        // has been drained, and only it reached the classifier.
        SearchSettings {
            sentiment_enabled: true,
            ..SearchSettings::default()
        }
        .save(&app.db, app.state.cache())
        .await
        .unwrap();
        let (status, _) = followup(app, "great, thanks again").await;
        assert_eq!(status, StatusCode::OK);

        let after = labels_change_from(app, before).await;
        assert_eq!(after, (before.0 + 1, before.1, before.2));
        assert_eq!(
            stub.classifier_calls.load(Ordering::SeqCst),
            1,
            "the follow-up asked with sentiment off was never classified"
        );
    });
}

/// (d) The follow-up text is written to no table.
#[test]
fn no_followup_text_is_stored_anywhere() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SERIAL.lock().await);
        let stub = setup(app, true).await;
        stub.gate.add_permits(1);
        *stub.label.lock().unwrap() = "satisfied".to_string();
        let before = labels_today(app).await;

        let marker = format!("zqxprivate{}", Uuid::now_v7().simple());
        let (status, _) = followup(app, &format!("is {marker} near the venue?")).await;
        assert_eq!(status, StatusCode::OK);
        labels_change_from(app, before).await;

        let columns: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_name::text, column_name::text FROM information_schema.columns \
             WHERE table_schema = 'public' \
               AND data_type IN ('text', 'character varying', 'jsonb', 'json')",
        )
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert!(!columns.is_empty());
        for (table, column) in columns {
            // The names come from the catalog, not from input.
            let sql = format!("SELECT count(*) FROM \"{table}\" WHERE \"{column}\"::text LIKE $1");
            let found: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(format!("%{marker}%"))
                .fetch_one(&app.db)
                .await
                .unwrap();
            assert_eq!(found, 0, "follow-up text found in {table}.{column}");
        }
    });
}
