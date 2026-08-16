#![allow(clippy::unwrap_used, clippy::expect_used)]
//! argus-m4 notification integration: drives the **real** `plugins/argus` wasm
//! through the **real** `TapDispatcher` and queue-v2 drain over Postgres +
//! Redis, from a decided article to a dispatched notification.
//!
//! # What this proves, and what it cannot
//!
//! It proves the whole chain to the moment of transmission: analyze → embed →
//! cluster → summarize → the notification trigger → the outbox row → the
//! dispatcher → a per-channel delivery row **carrying the exact payload that was
//! handed to the transport**. The golden assertions below are made against what
//! the real pipeline really produced, not against a hand-built fixture.
//!
//! It cannot prove transmission itself. The p11i SSRF fence blocks loopback and
//! every RFC-1918 range at the URL-string layer and again at the resolver layer
//! (`crates/kernel/src/host/http.rs`, `check_url_policy` + `ValidatingResolver`),
//! with no env-gated allowance, so a fixture webhook receiver on this machine is
//! unreachable from the `http` host by construction. That is **G-SSRF-LOCAL**,
//! accepted at CLOSE 05. The delivery outcome asserted here is therefore the
//! clean per-channel `blocked` state — which is itself a scope requirement — and
//! the transmission half is proved by the `argus-core` dispatch tests against an
//! in-memory transport.
//!
//! (The fixture *AI* provider below does reach loopback. That works only because
//! `ai-request` never re-validates a provider's base URL — G-AI-BASEURL-UNCHECKED,
//! disclosed in `M2-FRICTION.md`. It is a gap on the AI path and does not help
//! the notification path.)
//!
//! [`live_webhook_smoke_run`] is the manual counterpart: set
//! `ARGUS_E2E_WEBHOOK_URL` to a real external receiver and it asserts a real
//! `delivered`.
//!
//! Requires Postgres + Redis. Build the plugin first:
//!   cargo build -p argus --target wasm32-wasip1 --release \
//!     && cp target/wasm32-wasip1/release/argus.wasm plugins/argus/

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::PgPool;
use uuid::Uuid;

use trovato_kernel::content::ContentTypeRegistry;
use trovato_kernel::cron::CronService;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::ai_provider::AiProviderService;
use trovato_kernel::services::ai_token_budget::AiTokenBudgetService;
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const PLUGIN: &str = "argus";

/// The Live stage every seeded Item belongs to.
const LIVE_STAGE: &str = "0193a5a0-0000-7000-8000-000000000001";

/// A loopback webhook target. The SSRF fence refuses it, which is the point:
/// this is what an operator's misconfigured internal URL looks like from the
/// plugin's side.
const BLOCKED_WEBHOOK: &str = "http://127.0.0.1:9/argus-hook";

/// A loopback ntfy server.
///
/// **Not a detail.** Left at the default, these tests would publish to the
/// public `ntfy.sh` on every CI run — a test suite must not post to somebody
/// else's service. Pointing the server at loopback keeps the ntfy renderer
/// fully exercised (the URL it builds and the JSON body it sends are both
/// asserted below) while the fence stops the request at the door.
const BLOCKED_NTFY_SERVER: &str = "http://127.0.0.1:9";

/// The ntfy topic the seeded phone channel publishes to.
const NTFY_TOPIC: &str = "argus-e2e";

static SERIAL: Mutex<()> = Mutex::new(());

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});

fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    RT.block_on(body);
}

static DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

fn dispatcher() -> Arc<TapDispatcher> {
    DISPATCHER
        .get_or_init(|| {
            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
            runtime
                .load_plugin(&plugins_dir().join(PLUGIN))
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to load '{PLUGIN}': {e:#}\n\
                         build it: cargo build -p {PLUGIN} --target wasm32-wasip1 --release \
                         && cp target/wasm32-wasip1/release/{PLUGIN}.wasm plugins/{PLUGIN}/"
                    )
                });
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

/// Connect, run kernel migrations, and apply the argus schema (idempotent).
async fn fresh_pool() -> PgPool {
    dotenvy::dotenv().ok();
    // The plugin reports through `host::log`, which the kernel emits as a
    // tracing event. Without a subscriber a failing stage is a silent trap, so
    // `RUST_LOG=argus=debug,trovato_kernel=debug` is worth having wired up.
    static LOGGING: std::sync::Once = std::sync::Once::new();
    LOGGING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init();
    });
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run kernel migrations");
    for migration in [
        "001_argus_schema.sql",
        "003_argus_intelligence.sql",
        "004_argus_reader.sql",
        "005_argus_notify.sql",
    ] {
        let sql =
            std::fs::read_to_string(plugins_dir().join(format!("{PLUGIN}/migrations/{migration}")))
                .unwrap_or_else(|e| panic!("read {migration}: {e}"));
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("apply {migration}: {e}"));
    }
    ContentTypeRegistry::new(pool.clone(), std::time::Duration::from_secs(60))
        .sync_from_plugins(&dispatcher())
        .await
        .expect("register argus content types");
    pool
}

/// Reset argus state so a test starts clean.
async fn reset(pool: &PgPool) {
    for stmt in [
        "DELETE FROM plugin_queue WHERE plugin_name = 'argus'",
        "DELETE FROM item WHERE type IN ('argus_story', 'argus_feed', 'argus_topic', \
                                         'argus_notify_channel')",
        "DELETE FROM argus_read_state",
        "DELETE FROM argus_reactions",
        "DELETE FROM argus_subscriptions",
        "TRUNCATE argus_articles",
        "TRUNCATE argus_feeds",
        "TRUNCATE argus_topics",
        "TRUNCATE argus_state",
        "TRUNCATE argus_entities",
        "TRUNCATE argus_article_entities",
        "TRUNCATE argus_article_vectors",
        "TRUNCATE argus_stories",
        "TRUNCATE argus_cost_daily",
        "TRUNCATE argus_notify_events",
        "TRUNCATE argus_notify_deliveries",
        "TRUNCATE argus_notify_channels",
    ] {
        sqlx::query(stmt).execute(pool).await.unwrap();
    }
    // The variables host namespaces a plugin's keys as `plugin.<plugin>.<name>`
    // and argus names its own keys `argus.*`, so every key is doubly prefixed.
    sqlx::query("DELETE FROM site_config WHERE key LIKE 'plugin.argus.%'")
        .execute(pool)
        .await
        .unwrap();
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Set one of the plugin's site variables.
///
/// `name` is the plugin-facing key **without** its `argus.` prefix; the full row
/// key is `plugin.argus.argus.<name>`, because the variables host namespaces by
/// plugin and argus namespaces its own keys again.
async fn set_plugin_variable(pool: &PgPool, name: &str, value: &str) {
    sqlx::query(
        "INSERT INTO site_config (key, value, updated) VALUES ($1, $2, NOW()) \
         ON CONFLICT (key) DO UPDATE SET value = $2, updated = NOW()",
    )
    .bind(format!("plugin.argus.argus.{name}"))
    .bind(serde_json::json!(value))
    .execute(pool)
    .await
    .unwrap();
}

/// Switch the quiet window off for a test.
///
/// Without this a test's outcome would depend on the wall-clock hour it ran at:
/// the default window is 23:00–07:00, and a run at 03:00 would correctly defer
/// every normal-priority notification to the morning. Quiet hours are covered
/// exhaustively by the `argus-core` unit tests instead.
async fn disable_quiet_hours(pool: &PgPool) {
    set_plugin_variable(pool, "quiet_hours_start", "0").await;
    set_plugin_variable(pool, "quiet_hours_end", "0").await;
}

/// Insert a configuration Item directly, in the **flat** field shape the admin
/// content form writes and `argus_core::config` reads (`G-ITEM-FORM-MISMATCH`).
async fn seed_config_item(
    pool: &PgPool,
    item_type: &str,
    title: &str,
    fields: serde_json::Value,
) -> Uuid {
    let id = Uuid::now_v7();
    let author: Uuid = sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, \
                           promote, sticky, fields, stage_id, language, item_group_id) \
         VALUES ($1, $2, $3, $4, 1, $5, $5, 0, 0, $6, $7::uuid, 'en', $1)",
    )
    .bind(id)
    .bind(item_type)
    .bind(title)
    .bind(author)
    .bind(now())
    .bind(&fields)
    .bind(LIVE_STAGE)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn seed_topic(pool: &PgPool, priority: &str) -> Uuid {
    let id = Uuid::now_v7();
    seed_config_item(
        pool,
        "argus_topic",
        &format!("topic-{id}"),
        serde_json::json!({
            "field_relevance_prompt": "Is this about AI?",
            "field_relevance_threshold": 50,
            "field_notify_priority": priority,
        }),
    )
    .await
}

async fn seed_feed(pool: &PgPool, topic: Uuid, name: &str) -> Uuid {
    seed_config_item(
        pool,
        "argus_feed",
        name,
        serde_json::json!({
            "field_url": "https://example.test/feed.xml",
            "field_topic": topic.to_string(),
            "field_fetch_interval": 900,
        }),
    )
    .await
}

/// Seed a notification channel Item.
async fn seed_channel(pool: &PgPool, name: &str, kind: &str, target: &str, server: &str) -> Uuid {
    seed_config_item(
        pool,
        "argus_notify_channel",
        name,
        serde_json::json!({
            "field_kind": kind,
            "field_target": target,
            "field_server": server,
            "field_headers": r#"{"X-Argus-Test":"1"}"#,
            "field_min_priority": "normal",
            "field_events": "",
            "field_ntfy_priority": "",
        }),
    )
    .await
}

/// Seed the ntfy channel every test uses, pinned to a loopback server.
async fn seed_ntfy_channel(pool: &PgPool) -> Uuid {
    seed_channel(pool, "Ops phone", "ntfy", NTFY_TOPIC, BLOCKED_NTFY_SERVER).await
}

/// Seed the generic webhook channel every test uses, pinned to a loopback URL.
async fn seed_webhook_channel(pool: &PgPool) -> Uuid {
    seed_channel(pool, "Ops webhook", "webhook", BLOCKED_WEBHOOK, "").await
}

/// Seed a decide survivor, as the M1 pipeline would leave it, and enqueue its
/// analyze job.
async fn seed_decided_article(
    pool: &PgPool,
    feed: Uuid,
    topic: Uuid,
    url: &str,
    title: &str,
    score: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO argus_articles \
            (id, url, title, content, published_at, feed_id, topic_id, \
             relevance_score, pipeline_state, created, changed) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'decided', $9, $9)",
    )
    .bind(id)
    .bind(url)
    .bind(title)
    .bind(format!(
        "{title}. Datacenter revenue set a record this quarter."
    ))
    .bind(now() - 1800)
    .bind(feed)
    .bind(topic)
    .bind(score)
    .bind(now())
    .execute(pool)
    .await
    .unwrap();
    insert_job(
        pool,
        serde_json::json!({ "stage": "analyze", "id": id.to_string() }),
    )
    .await;
    id
}

async fn insert_job(pool: &PgPool, payload: serde_json::Value) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO plugin_queue \
            (plugin_name, queue_name, payload, created_at, priority, max_attempts, \
             next_attempt_at, status, attempts, locked_until) \
         VALUES ('argus', 'argus_stage', $1, $2, 0, 5, 0, 'ready', 0, 0) RETURNING id",
    )
    .bind(&payload)
    .bind(now())
    .fetch_one(pool)
    .await
    .unwrap()
}

fn cron_with_ai(pool: PgPool, disp: Arc<TapDispatcher>) -> Arc<CronService> {
    let redis = redis::Client::open("redis://127.0.0.1:6379").expect("redis client");
    let mut cron = CronService::new(redis, pool.clone());
    cron.set_tap_dispatcher(disp);
    cron.set_ai_providers(Arc::new(AiProviderService::new(pool.clone())));
    cron.set_ai_budgets(Arc::new(AiTokenBudgetService::new(pool)));
    Arc::new(cron)
}

async fn drain_until_idle(cron: &Arc<CronService>, max_rounds: usize) -> u64 {
    let mut succeeded = 0u64;
    let mut idle_rounds = 0;
    for _ in 0..max_rounds {
        let stats = cron.drain_plugin_queues().await.unwrap();
        succeeded += stats.succeeded;
        if stats.succeeded == 0 && stats.retried == 0 && stats.dead_lettered == 0 {
            idle_rounds += 1;
            if idle_rounds >= 3 {
                break;
            }
        } else {
            idle_rounds = 0;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    }
    succeeded
}

// ── the fixture AI provider ─────────────────────────────────────────────────

fn openai_completion(content: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "fixture-model",
        "choices": [{ "message": { "content": content }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
    })
}

fn analyze_fixture(title: &str) -> serde_json::Value {
    let entities: Vec<serde_json::Value> = title
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(char::is_uppercase) && w.chars().count() > 3)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .take(3)
        .map(|w| serde_json::json!({ "name": w, "type": "company" }))
        .collect();
    openai_completion(
        &serde_json::json!({
            "summary": format!("{title}. The report sets out what happened and who said so."),
            "critical_analysis": "The piece takes company guidance at face value.",
            "fallacy_analysis": "Appeal to authority in paragraph four.",
            "source_analysis": "Only the CFO is quoted; no analyst dissent appears.",
            "entities": entities
        })
        .to_string(),
    )
}

/// The story headline and narrative the golden assertions below check for.
const STORY_TITLE: &str = "Nvidia posts a record datacenter quarter";
const STORY_SUMMARY: &str =
    "Two outlets reported a record quarter.\n\nThey disagree on the margin.";

fn summarize_fixture() -> serde_json::Value {
    openai_completion(
        &serde_json::json!({ "title": STORY_TITLE, "summary": STORY_SUMMARY }).to_string(),
    )
}

/// Start a fixture OpenAI-compatible provider serving analyze, summarize and the
/// M4 change judge, told apart by their system prompts.
///
/// Reaches loopback only because `ai-request` does not re-validate a provider's
/// base URL (G-AI-BASEURL-UNCHECKED). See the module note.
async fn start_fixture_provider(judge_material: bool) -> String {
    use axum::{Json, Router, routing::post};

    let app = Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| async move {
            let system = body["messages"]
                .as_array()
                .and_then(|m| m.first())
                .and_then(|m| m["content"].as_str())
                .unwrap_or_default()
                .to_string();
            let user = body["messages"]
                .as_array()
                .and_then(|m| m.last())
                .and_then(|m| m["content"].as_str())
                .unwrap_or_default()
                .to_string();
            let title = user
                .lines()
                .find_map(|l| l.strip_prefix("Article title: "))
                .unwrap_or("Untitled")
                .to_string();
            if system.contains("decide whether an updated news summary") {
                Json(openai_completion(
                    &serde_json::json!({
                        "material": judge_material,
                        "reason": if judge_material { "a new development" } else { "reworded only" }
                    })
                    .to_string(),
                ))
            } else if system.contains("news editor") {
                Json(summarize_fixture())
            } else {
                Json(analyze_fixture(&title))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

async fn configure_fixture_provider(pool: &PgPool, base_url: &str) {
    let providers = serde_json::json!([{
        "id": "fixture",
        "label": "Fixture",
        "protocol": "open_ai_compatible",
        "base_url": base_url,
        "api_key_env": "",
        "models": [
            { "operation": "chat", "model": "fixture-model" }
        ],
        "rate_limit_rpm": 0,
        "enabled": true
    }]);
    let defaults = serde_json::json!({ "chat": "fixture" });
    let pricing = serde_json::json!({
        "models": { "fixture-model": { "input_per_1k": 0.001, "output_per_1k": 0.002 } }
    });
    for (key, value) in [
        ("ai_providers", providers),
        ("ai_defaults", defaults),
        ("ai_pricing", pricing),
    ] {
        sqlx::query(
            "INSERT INTO site_config (key, value, updated) VALUES ($1, $2, NOW()) \
             ON CONFLICT (key) DO UPDATE SET value = $2, updated = NOW()",
        )
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .unwrap();
    }
}

// ── reads ───────────────────────────────────────────────────────────────────

/// One outbox row: (event_type, priority, state, title, body, reason).
type EventRow = (String, String, String, String, String, Option<String>);

async fn events(pool: &PgPool) -> Vec<EventRow> {
    sqlx::query_as(
        "SELECT event_type, priority, state, title, body, reason \
         FROM argus_notify_events ORDER BY created, event_type",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// One delivery row: (channel_name, state, attempts, last_error, request_url,
/// request_body).
type DeliveryRow = (
    String,
    String,
    i32,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn deliveries(pool: &PgPool) -> Vec<DeliveryRow> {
    sqlx::query_as(
        "SELECT channel_name, state, attempts, last_error, request_url, request_body \
         FROM argus_notify_deliveries ORDER BY channel_name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn dead_jobs(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM plugin_queue WHERE plugin_name = 'argus' AND status = 'dead'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

// ── M4: the whole chain, article to notification ────────────────────────────

/// The milestone's end-to-end. Two decided articles become one summarized
/// story, the story becomes an outbox event, the event dispatches to two
/// channels, and both delivery rows carry the payload the real pipeline
/// rendered.
#[test]
fn the_pipeline_turns_a_summarized_story_into_a_dispatched_notification() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider(true).await;
        configure_fixture_provider(&pool, &base_url).await;
        // Coalescing a burst of joins into one summarize is what the rate limit
        // is for and is covered by the argus-core tests; this test is about the
        // notification, so both joins get to summarize.
        set_plugin_variable(&pool, "summarize_min_interval", "0").await;
        // Quiet hours would defer the dispatch to 07:00, which is correct
        // behaviour and covered by unit tests, but it would make this test's
        // outcome depend on when it was run.
        disable_quiet_hours(&pool).await;

        let topic = seed_topic(&pool, "normal").await;
        let feed_a = seed_feed(&pool, topic, "Reuters").await;
        let feed_b = seed_feed(&pool, topic, "Bloomberg").await;
        let ntfy = seed_ntfy_channel(&pool).await;
        let hook = seed_webhook_channel(&pool).await;

        seed_decided_article(
            &pool,
            feed_a,
            topic,
            "https://a.test/1",
            "Nvidia reports record datacenter revenue",
            85,
        )
        .await;
        seed_decided_article(
            &pool,
            feed_b,
            topic,
            "https://b.test/1",
            "Nvidia datacenter revenue hits a record",
            85,
        )
        .await;

        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 20).await;

        // ---- the story formed and was summarized -------------------------
        let (story_id, article_count): (Uuid, i32) =
            sqlx::query_as("SELECT id, article_count FROM argus_stories")
                .fetch_one(&pool)
                .await
                .expect("exactly one story");
        assert_eq!(article_count, 2);

        // ---- the outbox recorded the decision ----------------------------
        let rows_events = events(&pool).await;
        assert!(
            !rows_events.is_empty(),
            "the summarized story recorded no notification"
        );
        let (kind, priority, state, title, body, _) = &rows_events[0];
        assert_eq!(kind, "story.new", "the first synthesis is a new story");
        assert_eq!(priority, "normal", "an 85 on an ordinary topic is normal");
        assert_eq!(state, "sent");
        assert_eq!(title, STORY_TITLE);
        assert_eq!(body, STORY_SUMMARY);

        let subject: Option<Uuid> = sqlx::query_scalar(
            "SELECT subject_id FROM argus_notify_events ORDER BY created LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(subject, Some(story_id), "the event names its story");

        // ---- both channels have a delivery row ---------------------------
        let rows = deliveries(&pool).await;
        assert_eq!(rows.len(), 2, "one row per channel: {rows:?}");

        let (_, ntfy_state, ntfy_attempts, _, ntfy_url, ntfy_body) = &rows[0];
        assert_eq!(
            ntfy_state, "blocked",
            "the channel is pinned at loopback so the run stays offline"
        );
        assert_eq!(*ntfy_attempts, 1);
        // The URL the ntfy renderer built from the channel's server + topic.
        assert_eq!(
            ntfy_url.as_deref(),
            Some(format!("{BLOCKED_NTFY_SERVER}/{NTFY_TOPIC}").as_str())
        );
        let ntfy_payload: serde_json::Value =
            serde_json::from_str(ntfy_body.as_deref().unwrap()).unwrap();
        assert_eq!(ntfy_payload["title"], STORY_TITLE);
        assert!(ntfy_payload["message"].as_str().unwrap().contains("margin"));
        assert_eq!(
            ntfy_payload["priority"], 3,
            "a normal story is the default rung"
        );
        assert_eq!(ntfy_payload["tags"], serde_json::json!(["newspaper"]));

        let (_, hook_state, _, hook_error, hook_url, hook_body) = &rows[1];
        // The scope's explicit requirement: a target the SSRF fence refuses is a
        // clean per-channel error, and the payload it would have carried is
        // still on the row.
        assert_eq!(hook_state, "blocked");
        assert!(
            hook_error.as_deref().unwrap().contains("blocked"),
            "the error names the refusal: {hook_error:?}"
        );
        assert_eq!(hook_url.as_deref(), Some(BLOCKED_WEBHOOK));
        let hook_payload: serde_json::Value =
            serde_json::from_str(hook_body.as_deref().unwrap()).unwrap();
        assert_eq!(hook_payload["source"], "argus");
        assert_eq!(hook_payload["version"], 1);
        assert_eq!(hook_payload["event"], "story.new");
        assert_eq!(hook_payload["priority"], "normal");
        assert_eq!(hook_payload["title"], STORY_TITLE);
        assert_eq!(hook_payload["subject_id"], story_id.to_string());
        // One, not two: the rate limit is off for this test, so the story was
        // synthesized when it was founded (one member) and again when the second
        // report joined. The notification carries the story as it stood when the
        // reader was told about it.
        assert_eq!(hook_payload["data"]["article_count"], 1);

        // ---- the second synthesis did not notify twice --------------------
        // The story gained a member seconds later and was re-summarized. That is
        // an update, and the debounce window is what stops a reader being told
        // about the same story twice in an hour.
        let update = rows_events
            .iter()
            .find(|r| r.0 == "story.updated")
            .expect("the second synthesis recorded an update event");
        assert_eq!(update.2, "suppressed", "{update:?}");
        assert!(
            update.5.as_deref().unwrap_or_default().contains("debounce"),
            "suppressed for the documented reason: {update:?}"
        );

        // ---- a blocked channel is a per-channel error, not a job failure --
        assert_eq!(dead_jobs(&pool).await, 0, "nothing dead-lettered");
        let failures: Vec<(Uuid, i32, Option<String>)> = sqlx::query_as(
            "SELECT id, consecutive_failures, last_error FROM argus_notify_channels ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(failures.len(), 2, "both channels have health rows");
        assert!(failures.iter().all(|(_, n, e)| *n == 1 && e.is_some()));
        assert!(failures.iter().any(|(id, _, _)| *id == ntfy));
        assert!(failures.iter().any(|(id, _, _)| *id == hook));

        // ---- a blocked target is never retried ---------------------------
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM plugin_queue \
             WHERE plugin_name = 'argus' AND payload->>'stage' = 'notify'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, 0, "a permanent refusal schedules no retry");

        reset(&pool).await;
    });
}

#[test]
fn a_story_below_the_notify_threshold_tells_nobody() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider(true).await;
        configure_fixture_provider(&pool, &base_url).await;
        disable_quiet_hours(&pool).await;

        let topic = seed_topic(&pool, "normal").await;
        let feed = seed_feed(&pool, topic, "Reuters").await;
        seed_webhook_channel(&pool).await;
        // 40 is well under the default floor of 70.
        seed_decided_article(
            &pool,
            feed,
            topic,
            "https://a.test/1",
            "Minor update to a build tool",
            40,
        )
        .await;

        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 16).await;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM argus_stories")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1,
            "the story still forms; it just does not notify"
        );
        assert!(
            events(&pool).await.is_empty(),
            "a story under the floor records no notification"
        );
        assert!(deliveries(&pool).await.is_empty());

        reset(&pool).await;
    });
}

#[test]
fn a_high_priority_topic_notifies_at_any_score() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider(true).await;
        configure_fixture_provider(&pool, &base_url).await;
        disable_quiet_hours(&pool).await;

        let topic = seed_topic(&pool, "high").await;
        let feed = seed_feed(&pool, topic, "Reuters").await;
        seed_ntfy_channel(&pool).await;
        seed_decided_article(
            &pool,
            feed,
            topic,
            "https://a.test/1",
            "A quiet story on a loud topic",
            10,
        )
        .await;

        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 16).await;

        let rows = events(&pool).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].1, "high", "the topic's priority carried through");
        assert_eq!(rows[0].2, "sent");

        // A high-priority story climbs the ntfy ladder.
        let deliveries = deliveries(&pool).await;
        let payload: serde_json::Value =
            serde_json::from_str(deliveries[0].5.as_deref().unwrap()).unwrap();
        assert_eq!(payload["priority"], 4);

        reset(&pool).await;
    });
}

// ── M4: operator alerts from tap_cron ───────────────────────────────────────

/// Drive one `tap_cron` cycle through the real dispatcher and return its result.
async fn run_cron(pool: &PgPool) -> serde_json::Value {
    let disp = dispatcher();
    let state = RequestState::new(
        UserContext::background(),
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone()),
    );
    let input = serde_json::json!({ "timestamp": now() }).to_string();
    let result = disp
        .dispatch_to_plugin("tap_cron", &input, PLUGIN, state)
        .await
        .expect("argus implements tap_cron");
    serde_json::from_str(&result.output).expect("tap_cron returns JSON")
}

#[test]
fn a_failing_feed_and_a_stuck_queue_become_operator_alerts() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider(true).await;
        configure_fixture_provider(&pool, &base_url).await;
        disable_quiet_hours(&pool).await;

        let topic = seed_topic(&pool, "normal").await;
        let feed = seed_feed(&pool, topic, "A feed that stopped working").await;
        seed_webhook_channel(&pool).await;

        // Four consecutive failures, past the default threshold of three.
        sqlx::query(
            "INSERT INTO argus_feeds (id, failure_count, last_error, last_fetched_at, \
                                      created, changed) \
             VALUES ($1, 4, 'HTTP 503', $2, $2, $2)",
        )
        .bind(feed)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();

        // An eligible job that has been waiting far longer than the default
        // 900-second bound. This is what the queue-stuck check reads, and it
        // reads it out of the kernel's own `plugin_queue` table through
        // `query-raw` — the only introspection queue v2 offers a plugin
        // (G-QUEUE-NO-INTROSPECTION).
        sqlx::query(
            "INSERT INTO plugin_queue \
                (plugin_name, queue_name, payload, created_at, priority, max_attempts, \
                 next_attempt_at, status, attempts, locked_until) \
             VALUES ('argus', 'argus_stage', $1, $2, 0, 5, 0, 'ready', 0, 0)",
        )
        .bind(serde_json::json!({ "stage": "cluster", "id": Uuid::now_v7().to_string() }))
        .bind(now() - 7_200)
        .execute(&pool)
        .await
        .unwrap();

        let cron = cron_with_ai(pool.clone(), dispatcher());
        let result = run_cron(&pool).await;

        // A feed *state* row carries no configuration from M3 on, and the
        // legacy-config backfill must not try to read it as one — it did, and
        // errored on every tick until the query learned to tell the two apart.
        assert!(
            result["config_backfill"].get("error").is_none(),
            "the backfill choked on a state row: {result}"
        );

        let alerts = &result["alerts"];
        assert_eq!(alerts["feeds_failing"], 1, "cron result: {result}");
        assert_eq!(alerts["queue_alerted"], true, "cron result: {result}");
        assert_eq!(alerts["events_recorded"], 2);

        let rows = events(&pool).await;
        let kinds: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert!(kinds.contains(&"alert.feed_failing"), "{kinds:?}");
        assert!(kinds.contains(&"alert.queue_stuck"), "{kinds:?}");
        let feed_alert = rows
            .iter()
            .find(|r| r.0 == "alert.feed_failing")
            .expect("a feed alert");
        assert!(
            feed_alert.3.contains("A feed that stopped working"),
            "the alert names the feed: {}",
            feed_alert.3
        );
        assert!(feed_alert.4.contains("HTTP 503"));

        // A second cycle with nothing changed re-alerts nobody.
        let again = run_cron(&pool).await;
        assert_eq!(again["alerts"]["events_recorded"], 0, "{again}");
        assert_eq!(events(&pool).await.len(), 2);

        // The alerts dispatch through the same worker as a story.
        drain_until_idle(&cron, 12).await;
        let deliveries = deliveries(&pool).await;
        assert_eq!(deliveries.len(), 2, "both alerts reached the channel");
        assert!(deliveries.iter().all(|d| d.1 == "blocked"));
        assert_eq!(dead_jobs(&pool).await, 0);

        reset(&pool).await;
    });
}

#[test]
fn the_alert_pass_can_be_switched_off() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        set_plugin_variable(&pool, "alerts_enabled", "off").await;

        let topic = seed_topic(&pool, "normal").await;
        let feed = seed_feed(&pool, topic, "A feed that stopped working").await;
        sqlx::query(
            "INSERT INTO argus_feeds (id, failure_count, last_error, created, changed) \
             VALUES ($1, 9, 'HTTP 503', $2, $2)",
        )
        .bind(feed)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();

        let result = run_cron(&pool).await;

        assert_eq!(result["alerts"]["events_recorded"], 0, "{result}");
        assert!(events(&pool).await.is_empty());

        reset(&pool).await;
    });
}

// ── M4: the change judge over the real AI path ──────────────────────────────

#[test]
fn a_re_summarized_story_the_judge_calls_immaterial_notifies_nobody_twice() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        // The judge says "reworded only" for the second synthesis.
        let base_url = start_fixture_provider(false).await;
        configure_fixture_provider(&pool, &base_url).await;
        set_plugin_variable(&pool, "summarize_min_interval", "0").await;
        disable_quiet_hours(&pool).await;

        let topic = seed_topic(&pool, "normal").await;
        let feed_a = seed_feed(&pool, topic, "Reuters").await;
        let feed_b = seed_feed(&pool, topic, "Bloomberg").await;
        seed_webhook_channel(&pool).await;

        seed_decided_article(
            &pool,
            feed_a,
            topic,
            "https://a.test/1",
            "Nvidia reports record datacenter revenue",
            85,
        )
        .await;
        seed_decided_article(
            &pool,
            feed_b,
            topic,
            "https://b.test/1",
            "Nvidia datacenter revenue hits a record",
            85,
        )
        .await;

        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 20).await;

        let rows = events(&pool).await;
        // The first synthesis is a new story and goes out. The second is an
        // update the judge dismissed — or was debounced before it got that far.
        // Either way exactly one notification was sent.
        let sent = rows.iter().filter(|r| r.2 == "sent").count();
        assert_eq!(sent, 1, "one notification for one story: {rows:?}");
        if let Some(update) = rows.iter().find(|r| r.0 == "story.updated") {
            assert_eq!(
                update.2, "suppressed",
                "the update was not sent: {update:?}"
            );
            assert!(
                update.5.is_some(),
                "a suppressed event records why: {update:?}"
            );
        }

        // The judge is a real call and is counted against the day's spend, under
        // its own stage. M2's fence: notification spend is spend.
        let notify_spend: Option<(i32, f64)> = sqlx::query_as(
            "SELECT calls, cost_usd FROM argus_cost_daily WHERE stage = 'argus_notify'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        if let Some((calls, usd)) = notify_spend {
            assert!(calls >= 1, "the judge call was counted");
            assert!(usd > 0.0, "and priced from the response");
        }

        assert_eq!(dead_jobs(&pool).await, 0);
        reset(&pool).await;
    });
}

#[test]
fn the_judge_can_be_switched_off_without_losing_notifications() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider(true).await;
        configure_fixture_provider(&pool, &base_url).await;
        set_plugin_variable(&pool, "notify_judge", "off").await;
        disable_quiet_hours(&pool).await;

        let topic = seed_topic(&pool, "normal").await;
        let feed = seed_feed(&pool, topic, "Reuters").await;
        seed_ntfy_channel(&pool).await;
        seed_decided_article(
            &pool,
            feed,
            topic,
            "https://a.test/1",
            "Nvidia reports record datacenter revenue",
            85,
        )
        .await;

        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 16).await;

        let rows = events(&pool).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].2, "sent");
        // With the judge off, the notify stage spends nothing at all.
        let notify_calls: Option<i32> =
            sqlx::query_scalar("SELECT calls FROM argus_cost_daily WHERE stage = 'argus_notify'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(
            notify_calls.is_none_or(|c| c == 0),
            "the judge made no call: {notify_calls:?}"
        );

        reset(&pool).await;
    });
}

// ── the manual live run ─────────────────────────────────────────────────────

/// Deliver a real notification to a real external receiver.
///
/// The one thing the tests above cannot do (G-SSRF-LOCAL). Point it at anything
/// that accepts a POST and returns 2xx:
///
/// ```text
/// ARGUS_E2E_WEBHOOK_URL=https://webhook.site/<uuid> \
///   cargo test -p trovato-kernel --test argus_notify_test -- \
///   --ignored --nocapture live_webhook_smoke_run
/// ```
#[test]
#[ignore = "reaches the public internet; needs ARGUS_E2E_WEBHOOK_URL"]
fn live_webhook_smoke_run() {
    serial(async {
        let Ok(url) = std::env::var("ARGUS_E2E_WEBHOOK_URL") else {
            panic!("set ARGUS_E2E_WEBHOOK_URL to a receiver that accepts a POST");
        };
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider(true).await;
        configure_fixture_provider(&pool, &base_url).await;
        disable_quiet_hours(&pool).await;

        let topic = seed_topic(&pool, "normal").await;
        let feed = seed_feed(&pool, topic, "Reuters").await;
        seed_channel(&pool, "Live webhook", "webhook", &url, "").await;
        seed_decided_article(
            &pool,
            feed,
            topic,
            "https://a.test/1",
            "Nvidia reports record datacenter revenue",
            85,
        )
        .await;

        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 20).await;

        let rows = deliveries(&pool).await;
        println!("live delivery rows: {rows:#?}");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1, "delivered",
            "the receiver did not accept it: {:?}",
            rows[0]
        );

        reset(&pool).await;
    });
}
