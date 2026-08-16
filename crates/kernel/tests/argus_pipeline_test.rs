#![allow(clippy::unwrap_used, clippy::expect_used)]
//! argus-m1 pipeline integration: drives the **real** `plugins/argus` wasm
//! through the **real** `TapDispatcher` and `CronService` queue drain over
//! Postgres + Redis. Proves the parts of the pipeline that need the live host:
//!
//!   - M1-9: `tap_cron` enqueues a due feed and advances the round-robin cursor.
//!   - M1-5: a fetch against an SSRF-blocked URL is handled as a clean per-feed
//!     failure (failure_count bumped, last_error set), not a worker crash —
//!     exercising the p11i SSRF hardening from the consumer side.
//!   - M1-6: the idempotent upsert leaves exactly one row under replay.
//!   - M1-8: a decide job with no AI provider dead-letters after its attempts
//!     are exhausted (transient failure → panic → retry/DLQ).
//!   - M1-10: an analyze job parks the article in `analyzing` through the real
//!     worker → db-host write path.
//!
//! Requires Postgres + Redis. Build the plugin first:
//!   cargo build -p argus --target wasm32-wasip1 --release \
//!     && cp target/wasm32-wasip1/release/argus.wasm plugins/argus/
//!
//! The drain claims by `plugin_name`, so all tests here share the one `argus`
//! plugin and are serialized on [`SERIAL`] / one shared runtime [`RT`], each
//! cleaning the queue + tables on entry.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::{PgPool, Row};
use uuid::Uuid;

use trovato_kernel::content::ContentTypeRegistry;
use trovato_kernel::cron::CronService;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::ai_provider::AiProviderService;
use trovato_kernel::services::ai_token_budget::AiTokenBudgetService;
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const PLUGIN: &str = "argus";

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
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run kernel migrations");
    for migration in [
        "001_argus_schema.sql",
        // M2 adds columns to M1 tables, so the order matters.
        "003_argus_intelligence.sql",
        // M3 relaxes argus_feeds to a state-only table and adds reader state.
        "004_argus_reader.sql",
    ] {
        let sql =
            std::fs::read_to_string(plugins_dir().join(format!("{PLUGIN}/migrations/{migration}")))
                .unwrap_or_else(|e| panic!("read {migration}: {e}"));
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("apply {migration}: {e}"));
    }
    // `item.type` is a foreign key into `item_type`, so an `argus_story` Item
    // cannot be created until the plugin's `tap_item_info` has been synced —
    // the same registration a plugin enable performs in production. M1 never
    // needed it because M1 never created a story.
    ContentTypeRegistry::new(pool.clone(), std::time::Duration::from_secs(60))
        .sync_from_plugins(&dispatcher())
        .await
        .expect("register argus content types");
    pool
}

fn cron_with(pool: PgPool, disp: Arc<TapDispatcher>) -> Arc<CronService> {
    let redis = redis::Client::open("redis://127.0.0.1:6379").expect("redis client");
    let mut cron = CronService::new(redis, pool);
    cron.set_tap_dispatcher(disp);
    Arc::new(cron)
}

/// A drain wired with the AI provider + budget services, so `ai-request`
/// reaches a configured provider instead of failing with `ERR_AI_NO_PROVIDER`.
fn cron_with_ai(pool: PgPool, disp: Arc<TapDispatcher>) -> Arc<CronService> {
    let redis = redis::Client::open("redis://127.0.0.1:6379").expect("redis client");
    let mut cron = CronService::new(redis, pool.clone());
    cron.set_tap_dispatcher(disp);
    cron.set_ai_providers(Arc::new(AiProviderService::new(pool.clone())));
    cron.set_ai_budgets(Arc::new(AiTokenBudgetService::new(pool)));
    Arc::new(cron)
}

/// Reset argus state so a test starts clean.
async fn reset(pool: &PgPool) {
    for stmt in [
        "DELETE FROM plugin_queue WHERE plugin_name = 'argus'",
        // Feeds and topics are Items from M3, so clearing them is a delete on
        // `item`, not a truncate of the legacy tables.
        "DELETE FROM item WHERE type IN ('argus_story', 'argus_feed', 'argus_topic')",
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
        // Site variables outlive a test, and a *failed* run cannot clean up
        // after itself — so the embed-route switch is cleared here rather than
        // only at the end of the test that sets it. Left set, it sends every
        // other test's embed stage at an AI provider they do not wire.
        "DELETE FROM site_config WHERE key = 'plugin.argus.argus.embed_model'",
    ] {
        sqlx::query(stmt).execute(pool).await.unwrap();
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The Live stage every seeded Item belongs to.
const LIVE_STAGE: &str = "0193a5a0-0000-7000-8000-000000000001";

/// Insert a configuration Item directly, returning its id.
///
/// From M3 a feed's and a topic's configuration is an Item (`M3-DESIGN.md`
/// Decision 1). The `fields` shape here is the **flat** one the admin content
/// form writes and `argus_core::config` reads — not the `{"value": …}` wrapper
/// the story sync uses (`G-ITEM-FORM-MISMATCH`).
async fn seed_config_item(
    pool: &PgPool,
    item_type: &str,
    title: &str,
    fields: serde_json::Value,
    published: bool,
) -> Uuid {
    let id = Uuid::now_v7();
    let author: Uuid = sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, \
                           promote, sticky, fields, stage_id, language, item_group_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $6, 0, 0, $7, $8::uuid, 'en', $1)",
    )
    .bind(id)
    .bind(item_type)
    .bind(title)
    .bind(author)
    .bind(if published { 1i16 } else { 0i16 })
    .bind(now())
    .bind(&fields)
    .bind(LIVE_STAGE)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn seed_topic(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    seed_config_item(
        pool,
        "argus_topic",
        &format!("topic-{id}"),
        serde_json::json!({
            "field_relevance_prompt": "Is this about AI?",
            "field_relevance_threshold": 50,
        }),
        true,
    )
    .await
}

async fn seed_feed(pool: &PgPool, topic: Uuid, url: &str, last_fetched: Option<i64>) -> Uuid {
    let id = seed_config_item(
        pool,
        "argus_feed",
        "Feed",
        serde_json::json!({
            "field_url": url,
            "field_topic": topic.to_string(),
            "field_fetch_interval": 900,
        }),
        true,
    )
    .await;
    // The state row is created on demand by the first fetch; seed it only when
    // the test needs a prior fetch time to schedule against.
    if let Some(at) = last_fetched {
        sqlx::query(
            "INSERT INTO argus_feeds (id, last_fetched_at, created, changed) \
             VALUES ($1, $2, $3, $3) \
             ON CONFLICT (id) DO UPDATE SET last_fetched_at = EXCLUDED.last_fetched_at",
        )
        .bind(id)
        .bind(at)
        .bind(now())
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

async fn seed_article(pool: &PgPool, topic: Uuid, url: &str, state: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO argus_articles (id, url, title, content, topic_id, pipeline_state, created, changed) \
         VALUES ($1, $2, 'Title', 'some article body text', $3, $4, $5, $5)",
    )
    .bind(id)
    .bind(url)
    .bind(topic)
    .bind(state)
    .bind(now())
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Insert a queue job for argus with explicit v2 fields; returns its id.
async fn insert_job(pool: &PgPool, payload: serde_json::Value, max_attempts: i32) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO plugin_queue \
            (plugin_name, queue_name, payload, created_at, priority, max_attempts, \
             next_attempt_at, status, attempts, locked_until) \
         VALUES ('argus', 'argus_stage', $1, $2, 0, $3, 0, 'ready', 0, 0) RETURNING id",
    )
    .bind(&payload)
    .bind(now())
    .bind(max_attempts)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn row_status(pool: &PgPool, id: i64) -> Option<(String, i32, Option<String>)> {
    sqlx::query("SELECT status, attempts, dead_reason FROM plugin_queue WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .map(|r| (r.get("status"), r.get("attempts"), r.get("dead_reason")))
}

// ── M1-9: tap_cron enqueues a due feed and advances the cursor ───────────────

#[test]
fn cron_enqueues_due_feed_and_advances_cursor() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;
        let feed = seed_feed(&pool, topic, "https://example.test/feed.xml", None).await;

        let disp = dispatcher();
        let state = RequestState::new(
            UserContext::background(),
            RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
                .with_plugin_runtime(disp.runtime().clone()),
        );
        let input = serde_json::json!({ "timestamp": now() }).to_string();
        disp.dispatch_to_plugin("tap_cron", &input, PLUGIN, state)
            .await
            .expect("argus implements tap_cron");

        // A single fetch job for the due feed.
        let rows = sqlx::query("SELECT payload FROM plugin_queue WHERE plugin_name = 'argus'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "one fetch job enqueued");
        let payload: serde_json::Value = rows[0].get("payload");
        assert_eq!(payload["stage"], "fetch");
        assert_eq!(payload["id"], feed.to_string());

        // Cursor advanced (one feed scanned).
        let cursor: Option<String> =
            sqlx::query_scalar("SELECT value FROM argus_state WHERE name = 'schedule_cursor'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert_eq!(cursor.as_deref(), Some("1"), "cursor advanced past 1 feed");

        reset(&pool).await;
    });
}

// ── M1-5: an SSRF-blocked fetch flags the feed, no crash ─────────────────────

#[test]
fn fetch_ssrf_blocked_flags_feed_without_crashing() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;
        // Link-local / cloud-metadata address: the p11i SSRF fence blocks it.
        let feed = seed_feed(
            &pool,
            topic,
            "http://169.254.169.254/latest/meta-data",
            None,
        )
        .await;
        insert_job(
            &pool,
            serde_json::json!({ "stage": "fetch", "id": feed.to_string() }),
            5,
        )
        .await;

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();
        assert_eq!(
            stats.succeeded, 1,
            "worker handled the block, did not crash"
        );

        // Job consumed, feed flagged.
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM plugin_queue WHERE plugin_name = 'argus'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0, "fetch job consumed (no retry storm)");

        let (failures, last_error): (i32, Option<String>) =
            sqlx::query_as("SELECT failure_count, last_error FROM argus_feeds WHERE id = $1")
                .bind(feed)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(failures, 1, "feed failure_count bumped");
        assert!(last_error.is_some(), "feed last_error recorded");

        reset(&pool).await;
    });
}

// ── M1-6: idempotent upsert leaves exactly one row under replay ──────────────

#[test]
fn article_upsert_is_idempotent() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;

        // The exact upsert the worker runs (INSERT ... ON CONFLICT (url) DO NOTHING).
        let upsert = "INSERT INTO argus_articles \
             (id, url, title, content, published_at, feed_id, topic_id, pipeline_state, content_hash, created, changed) \
             VALUES (gen_random_uuid(), $1, 'T', 'body', NULL, NULL, $2::uuid, 'fetched', 'h', 1, 1) \
             ON CONFLICT (url) DO NOTHING";
        let first = sqlx::query(upsert)
            .bind("https://x.test/dup")
            .bind(topic.to_string())
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected();
        let second = sqlx::query(upsert)
            .bind("https://x.test/dup")
            .bind(topic.to_string())
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(first, 1, "first insert creates the row");
        assert_eq!(second, 0, "replay inserts nothing (idempotent)");

        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM argus_articles WHERE url = 'https://x.test/dup'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "exactly one row after replay");

        reset(&pool).await;
    });
}

// ── M1-8: a decide job with no provider dead-letters ─────────────────────────

#[test]
fn decide_without_provider_dead_letters() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;
        let article = seed_article(&pool, topic, "https://x.test/decide", "fetched").await;
        // max_attempts = 1: the first (only) attempt fails → dead-letter.
        let job = insert_job(
            &pool,
            serde_json::json!({ "stage": "decide", "id": article.to_string() }),
            1,
        )
        .await;

        // No AI provider is wired into the drain, so ai-request fails → the
        // decide stage returns a transient error → the worker panics → attempt
        // fails → attempts (1) >= max_attempts (1) → dead.
        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();
        assert_eq!(
            stats.succeeded, 0,
            "decide could not succeed without a provider"
        );

        let (status, attempts, dead_reason) = row_status(&pool, job).await.expect("job row exists");
        assert_eq!(status, "dead", "poison decide dead-letters");
        assert_eq!(attempts, 1);
        assert!(dead_reason.is_some(), "dead_reason recorded on the DLQ row");

        reset(&pool).await;
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// M2: the intelligence stages, through the real wasm
// ═══════════════════════════════════════════════════════════════════════════

/// Seed an article that has already been through decide and analyze, so the
/// embed/cluster stages have something real to work on without an AI provider.
async fn seed_analyzed_article(
    pool: &PgPool,
    topic: Uuid,
    feed: Uuid,
    url: &str,
    title: &str,
    summary: &str,
    published_at: i64,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO argus_articles \
            (id, url, title, content, summary, published_at, feed_id, topic_id, \
             relevance_score, pipeline_state, created, changed) \
         VALUES ($1, $2, $3, $4, $4, $5, $6, $7, 80, 'analyzed', $8, $8)",
    )
    .bind(id)
    .bind(url)
    .bind(title)
    .bind(summary)
    .bind(published_at)
    .bind(feed)
    .bind(topic)
    .bind(now())
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Drain repeatedly, so a chain of stages that enqueue each other runs to
/// completion. Returns the total number of jobs that succeeded.
///
/// Sleeps between rounds and requires several consecutive empty rounds before
/// stopping, because some jobs come back on a **delay** rather than
/// immediately — a cluster job that lost the clustering lease re-enqueues
/// itself a couple of seconds out. Draining in a tight loop would declare the
/// queue idle while that job was still waiting its turn.
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

// ── M2: embed → cluster builds a story Item through the real host ────────────

#[test]
fn embed_and_cluster_build_a_story_item() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;
        let feed_a = seed_feed(&pool, topic, "https://a.test/feed.xml", None).await;
        let feed_b = seed_feed(&pool, topic, "https://b.test/feed.xml", None).await;

        // Two independent reports of one event.
        let first = seed_analyzed_article(
            &pool,
            topic,
            feed_a,
            "https://a.test/1",
            "Nvidia reports record datacenter revenue",
            "Nvidia reported record datacenter revenue for the quarter, beating guidance.",
            now() - 3600,
        )
        .await;
        let second = seed_analyzed_article(
            &pool,
            topic,
            feed_b,
            "https://b.test/1",
            "Nvidia datacenter revenue hits a record",
            "Datacenter revenue at Nvidia reached a record this quarter, above guidance.",
            now() - 1800,
        )
        .await;

        for article in [first, second] {
            insert_job(
                &pool,
                serde_json::json!({ "stage": "embed", "id": article.to_string() }),
                5,
            )
            .await;
        }

        // No AI provider is needed: embed is a lexical vector and cluster is
        // pure arithmetic. Only summarize spends, and it dead-letters here,
        // which is asserted separately below.
        let cron = cron_with(pool.clone(), dispatcher());
        drain_until_idle(&cron, 12).await;

        // Both articles were embedded with the same recipe.
        let vectors: Vec<(String, i32)> = sqlx::query_as(
            "SELECT recipe, dim FROM argus_article_vectors WHERE article_id = ANY($1)",
        )
        .bind(vec![first, second])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(vectors.len(), 2, "both articles embedded");
        assert!(vectors.iter().all(|(r, d)| r == "lex-v1/256" && *d == 256));

        // Exactly one story, holding both articles.
        let stories: Vec<(Uuid, i32, bool)> =
            sqlx::query_as("SELECT id, article_count, is_active FROM argus_stories")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(stories.len(), 1, "two reports of one event, one story");
        let (story_id, count, active) = stories[0];
        assert_eq!(count, 2, "both articles counted as members");
        assert!(active);

        // The story row's id IS an `argus_story` Item, so the M1 reverse
        // reference (articles filtered by story_id) resolves from the Item.
        let (item_type, item_status): (String, i16) =
            sqlx::query_as("SELECT type, status FROM item WHERE id = $1")
                .bind(story_id)
                .fetch_one(&pool)
                .await
                .expect("the story row's id is a real Item");
        assert_eq!(item_type, "argus_story");
        assert_eq!(item_status, 1, "stories are published");

        let fields: serde_json::Value = sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
            .bind(story_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(fields["field_article_count"]["value"], 2);
        assert_eq!(fields["field_is_active"]["value"], true);

        let states: Vec<(String, Option<Uuid>)> = sqlx::query_as(
            "SELECT pipeline_state, story_id FROM argus_articles ORDER BY published_at",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(states.iter().all(|(s, _)| s == "complete"));
        assert!(states.iter().all(|(_, sid)| *sid == Some(story_id)));

        reset(&pool).await;
    });
}

// ── K1 fix 2: the semantic embed route, end to end through the host ──────────

/// **The consumer validation for G-AI-EMBED-UNROUTED.** Argus asks the kernel
/// for `operation: Embedding`; the host must reach an *embeddings* endpoint,
/// return a vector, and the cluster stage must then behave on those vectors.
///
/// Before `KERNEL_API_VERSION (0,99)` this was impossible: `execute_ai_request`
/// branched on protocol only, `build_openai_request` never read
/// `request.input`, and `parse_openai_response` read
/// `choices[0].message.content` — so the plugin got an empty string where it
/// expected a float array, which is why M2 shipped lexical vectors instead.
#[test]
fn the_semantic_embed_route_reaches_the_provider_and_clusters() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider().await;
        configure_fixture_provider(&pool, &base_url).await;
        // Switch Argus off the lexical route (M2 Decision 1).
        set_plugin_variable(&pool, "argus.embed_model", "fixture-embed").await;

        let topic = seed_topic(&pool).await;
        let feed_a = seed_feed(&pool, topic, "https://a.test/feed.xml", None).await;
        let feed_b = seed_feed(&pool, topic, "https://b.test/feed.xml", None).await;

        // Two reports of one event that share almost **no vocabulary** — the
        // case lexical vectors miss and semantic embeddings are bought for.
        let first = seed_analyzed_article(
            &pool,
            topic,
            feed_a,
            "https://a.test/1",
            "Nvidia reports record datacenter revenue",
            "Nvidia posted record datacenter revenue this quarter, beating guidance.",
            now() - 3600,
        )
        .await;
        let second = seed_analyzed_article(
            &pool,
            topic,
            feed_b,
            "https://b.test/1",
            "Chipmaker beats guidance as GPU sales climb",
            "The chipmaker's graphics business drove earnings past expectations.",
            now() - 1800,
        )
        .await;
        // An unrelated event, to prove the route discriminates rather than
        // collapsing everything into one story.
        let unrelated = seed_analyzed_article(
            &pool,
            topic,
            feed_a,
            "https://a.test/2",
            "Magnitude 6 quake strikes the coast",
            "A seismic tremor was recorded offshore early on Tuesday.",
            now() - 2400,
        )
        .await;

        for article in [first, second, unrelated] {
            insert_job(
                &pool,
                serde_json::json!({ "stage": "embed", "id": article.to_string() }),
                5,
            )
            .await;
        }

        // The usage log is not part of `reset` (it is kernel-owned and other
        // tests in this file write to it), so clear argus's rows to make the
        // per-article call count below an exact assertion rather than a floor.
        sqlx::query("DELETE FROM ai_usage_log WHERE plugin_name = 'argus'")
            .execute(&pool)
            .await
            .unwrap();

        // `cron_with_ai`, not `cron_with`: the semantic route needs the AI
        // provider + budget services wired, exactly as the lexical route did not.
        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 14).await;

        // Every article carries a *semantic* vector: the recipe names the model,
        // and the dimension is the provider's, not `argus.vector_dim`.
        let vectors: Vec<(Uuid, String, i32)> = sqlx::query_as(
            "SELECT article_id, recipe, dim FROM argus_article_vectors \
             WHERE article_id = ANY($1)",
        )
        .bind(vec![first, second, unrelated])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(vectors.len(), 3, "all three embedded, got {vectors:?}");
        assert!(
            vectors
                .iter()
                .all(|(_, r, d)| r == "sem-v1/fixture-embed" && *d == 4),
            "expected the provider's vector under the semantic recipe, got {vectors:?}"
        );

        // The vector is a real float array, not a chat completion's empty
        // content — the precise symptom the finding described.
        let raw: String =
            sqlx::query_scalar("SELECT vector FROM argus_article_vectors WHERE article_id = $1")
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        let parsed: Vec<f32> = serde_json::from_str(&raw).expect("stored vector parses as floats");
        assert_eq!(parsed.len(), 4);
        assert!(
            parsed.iter().any(|v| *v != 0.0),
            "a zero vector means nothing was embedded: {parsed:?}"
        );

        // The kernel logged it as an *embedding* call, with the embedding
        // model — the accounting path follows the routing.
        let logged: Vec<(String, String)> = sqlx::query_as(
            "SELECT operation, model FROM ai_usage_log WHERE plugin_name = 'argus' \
             AND operation = 'Embedding'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(logged.len(), 3, "one embedding call per article");
        assert!(logged.iter().all(|(_, m)| m == "fixture-embed"));

        // And clustering behaves on those vectors: the two vocabularies of one
        // event join, the unrelated one does not.
        let members: Vec<(Uuid, Option<Uuid>)> =
            sqlx::query_as("SELECT id, story_id FROM argus_articles WHERE id = ANY($1)")
                .bind(vec![first, second, unrelated])
                .fetch_all(&pool)
                .await
                .unwrap();
        let story_of = |id: Uuid| members.iter().find(|(a, _)| *a == id).and_then(|(_, s)| *s);
        let (a, b, c) = (story_of(first), story_of(second), story_of(unrelated));
        assert!(
            a.is_some() && b.is_some() && c.is_some(),
            "all filed: {members:?}"
        );
        assert_eq!(
            a, b,
            "two reports of one event must share a story on the semantic route"
        );
        assert_ne!(c, a, "an unrelated event must not join it");

        // **G-ITEM-NO-EMBED, closed.** Each story is an `argus_story` Item
        // written through the `save-item` host, which used to bypass
        // `ItemService::index_item` entirely — so a plugin-created story was
        // full-text findable but had no embedding, and was therefore invisible
        // to a `SemanticSimilarity` gather. That negated the reason `argus_story`
        // is an Item at all. The host now records the same "needs embedding"
        // intent the kernel save path records.
        let pending: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT s.item_id, s.state FROM item_embed_status s \
             JOIN item i ON i.id = s.item_id WHERE i.type = 'argus_story'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            pending.len(),
            2,
            "every plugin-created story Item must be queued for embedding, got {pending:?}"
        );
        assert!(pending.iter().all(|(_, state)| state == "pending"));

        // `enqueue_embed_job` writes the queue row *before* marking the item
        // pending, so `pending` above already implies the enqueue succeeded.
        // The row itself is not asserted: the drain in this test claims across
        // plugins and may legitimately have consumed it already.

        reset(&pool).await;
    });
}

// ── M2: an unrelated article starts its own story ────────────────────────────

#[test]
fn an_unrelated_article_starts_a_second_story() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;
        let feed_a = seed_feed(&pool, topic, "https://a.test/feed.xml", None).await;
        let feed_b = seed_feed(&pool, topic, "https://b.test/feed.xml", None).await;

        let chips = seed_analyzed_article(
            &pool,
            topic,
            feed_a,
            "https://a.test/chips",
            "Nvidia reports record datacenter revenue",
            "Nvidia reported record datacenter revenue, beating its own guidance.",
            now() - 3600,
        )
        .await;
        let flood = seed_analyzed_article(
            &pool,
            topic,
            feed_b,
            "https://b.test/flood",
            "Flooding closes the coastal highway",
            "Heavy rain closed the coastal highway for a second day, stranding drivers.",
            now() - 1800,
        )
        .await;

        for article in [chips, flood] {
            insert_job(
                &pool,
                serde_json::json!({ "stage": "embed", "id": article.to_string() }),
                5,
            )
            .await;
        }
        let cron = cron_with(pool.clone(), dispatcher());
        drain_until_idle(&cron, 12).await;

        let story_count: i64 = sqlx::query_scalar("SELECT count(*) FROM argus_stories")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(story_count, 2, "unrelated reports do not merge");

        reset(&pool).await;
    });
}

// ── M2: maintenance retires idle stories and reclaims old bodies ─────────────

#[test]
fn cron_maintenance_retires_stale_stories_and_purges_old_content() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;
        let feed = seed_feed(&pool, topic, "https://a.test/feed.xml", None).await;

        let day = 86_400_i64;
        let old_publish = now() - 300 * day;
        let article = seed_analyzed_article(
            &pool,
            topic,
            feed,
            "https://a.test/old",
            "Old news",
            "Something happened a long time ago.",
            old_publish,
        )
        .await;
        // An idle story holding it.
        let story = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO argus_stories \
                (id, topic_id, centroid, dim, recipe, article_count, first_article_at, \
                 last_article_at, is_active, created, changed) \
             VALUES ($1, $2, '[]', 0, 'lex-v1/256', 1, $3, $3, true, $4, $4)",
        )
        .bind(story)
        .bind(topic)
        .bind(old_publish)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE argus_articles SET story_id = $2, pipeline_state = 'complete' WHERE id = $1",
        )
        .bind(article)
        .bind(story)
        .execute(&pool)
        .await
        .unwrap();

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
        let report: serde_json::Value =
            serde_json::from_str(&result.output).expect("tap_cron returns JSON");

        assert_eq!(
            report["maintenance"]["stories_retired"], 1,
            "an idle story is retired: {report}"
        );
        assert_eq!(
            report["maintenance"]["articles_purged"], 1,
            "an old terminal article's body is reclaimed: {report}"
        );

        let (content, purged_at, still_a_member): (String, Option<i64>, Option<Uuid>) =
            sqlx::query_as(
                "SELECT content, content_purged_at, story_id FROM argus_articles WHERE id = $1",
            )
            .bind(article)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(content, "", "body text reclaimed");
        assert!(purged_at.is_some());
        assert_eq!(
            still_a_member,
            Some(story),
            "metadata and story membership survive the purge"
        );

        let active: bool = sqlx::query_scalar("SELECT is_active FROM argus_stories WHERE id = $1")
            .bind(story)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!active, "the story stops accepting articles");

        reset(&pool).await;
    });
}

// ── M2: the whole chain against a fixture AI provider ────────────────────────

/// A canned analyze response, as an OpenAI-compatible chat completion.
///
/// The entity list is derived from the article's own title (its capitalized
/// words), not hard-coded: a fixture that named the same entities for every
/// article would make every article share every entity, and clustering over a
/// real feed set would collapse into one story for reasons that had nothing to
/// do with the clustering logic.
fn analyze_fixture(title: &str) -> serde_json::Value {
    let entities: Vec<serde_json::Value> = title
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(char::is_uppercase) && w.chars().count() > 3)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .take(3)
        .map(|w| serde_json::json!({ "name": w, "type": "company" }))
        .collect();
    let content = serde_json::json!({
        "summary": format!("{title}. The report sets out what happened and who said so."),
        "critical_analysis": "The piece takes company guidance at face value.",
        "fallacy_analysis": "Appeal to authority in paragraph four.",
        "source_analysis": "Only the CFO is quoted; no analyst dissent appears.",
        "entities": entities
    })
    .to_string();
    openai_completion(&content)
}

/// A canned summarize response.
fn summarize_fixture() -> serde_json::Value {
    let content = serde_json::json!({
        "title": "Nvidia posts a record datacenter quarter",
        "summary": "Two outlets reported a record quarter.\n\nThey disagree on the margin."
    })
    .to_string();
    openai_completion(&content)
}

/// Wrap `content` in an OpenAI-compatible chat-completion envelope.
fn openai_completion(content: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "fixture-model",
        "choices": [{ "message": { "content": content }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
    })
}

/// The analyze stage's five output columns, as the chain test reads them back.
type AnalysisColumns = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A canned decide response.
///
/// The score is derived from the title so the same article always scores the
/// same, and so a real feed set produces a realistic spread of keeps and
/// discards rather than a uniform verdict.
fn decide_fixture(title: &str) -> serde_json::Value {
    let score = title.bytes().map(u32::from).sum::<u32>() % 101;
    openai_completion(
        &serde_json::json!({ "score": score, "reason": "fixture relevance filter" }).to_string(),
    )
}

/// Start a fixture OpenAI-compatible provider on loopback and return its base
/// URL. It tells the stages apart by their system prompts, so one server serves
/// all three.
///
/// **This depends on the `ai-request` path not re-validating a provider's
/// `base_url`.** `AiProviderService::embed` and `test_connection` both call
/// `validate_base_url` (which blocks loopback); the chat path does not. That
/// asymmetry is reported as **G-AI-BASEURL-UNCHECKED** in `M2-FRICTION.md`; if
/// it is ever closed, this fixture needs a non-loopback bind or an env-gated
/// test allowance.
async fn start_fixture_provider() -> String {
    use axum::{Json, Router, routing::post};

    let app = Router::new().route(
        "/chat/completions",
        post(|Json(body): Json<serde_json::Value>| async move {
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
            if system.contains("news editor") {
                Json(summarize_fixture())
            } else if system.contains("relevance filter") {
                Json(decide_fixture(&title))
            } else {
                Json(analyze_fixture(&title))
            }
        }),
    );
    // K1 fix 2: a real embeddings endpoint, which the host could not reach
    // before `KERNEL_API_VERSION (0,99)` — `operation: Embedding` was posted to
    // /chat/completions with an empty `messages` array (G-AI-EMBED-UNROUTED).
    //
    // The "model" is deliberately crude but *semantic in the right way*: it
    // scores the input on a handful of topic axes rather than on shared
    // vocabulary, so two reports of one event written in different words land
    // close together — which is the property the lexical route cannot express
    // and the whole reason to route embeddings at all.
    let app = app.route(
        "/embeddings",
        post(|Json(body): Json<serde_json::Value>| async move {
            let input = body["input"].as_str().unwrap_or_default().to_lowercase();
            let axis = |words: &[&str]| -> f64 {
                let hits = words.iter().filter(|w| input.contains(**w)).count();
                if hits == 0 {
                    0.0
                } else {
                    1.0 + hits as f64 * 0.01
                }
            };
            // Two vocabularies for one event on axis 0, an unrelated event on 1.
            let vector = vec![
                axis(&["nvidia", "datacenter", "gpu", "chipmaker", "graphics"]),
                axis(&["quake", "earthquake", "tremor", "seismic"]),
                axis(&["revenue", "earnings", "quarter", "guidance"]) * 0.3,
                0.05,
            ];
            Json(serde_json::json!({
                "object": "list",
                "model": "fixture-embed",
                "data": [{ "index": 0, "embedding": vector }],
                "usage": { "prompt_tokens": 12, "total_tokens": 12 }
            }))
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Set one of the plugin's site variables, as the `variables` host reads them.
async fn set_plugin_variable(pool: &PgPool, name: &str, value: &str) {
    sqlx::query(
        "INSERT INTO site_config (key, value, updated) VALUES ($1, $2, NOW()) \
         ON CONFLICT (key) DO UPDATE SET value = $2, updated = NOW()",
    )
    .bind(format!("plugin.argus.{name}"))
    .bind(serde_json::json!(value))
    .execute(pool)
    .await
    .unwrap();
}

/// Point the kernel's AI config at the fixture provider and price its model, so
/// `AiResponse.cost_estimate` comes back non-null and the plugin can account
/// its own spend from the response (the p11j companion fix).
async fn configure_fixture_provider(pool: &PgPool, base_url: &str) {
    let providers = serde_json::json!([{
        "id": "fixture",
        "label": "Fixture",
        "protocol": "open_ai_compatible",
        "base_url": base_url,
        "api_key_env": "",
        "models": [
            { "operation": "chat", "model": "fixture-model" },
            { "operation": "embedding", "model": "fixture-embed" }
        ],
        "rate_limit_rpm": 0,
        "enabled": true
    }]);
    let defaults = serde_json::json!({ "chat": "fixture", "embedding": "fixture" });
    // $0.001 per 1k input, $0.002 per 1k output: the fixture's 1000/500 tokens
    // therefore cost exactly $0.002 per call, which the assertions rely on.
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

#[test]
fn the_whole_chain_turns_decided_articles_into_one_summarized_story() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider().await;
        configure_fixture_provider(&pool, &base_url).await;
        // Drop the summarize rate limit here only. A story summarized when it
        // had one member is correctly *not* re-summarized seconds later when
        // the second arrives — that coalescing is what the limit is for, and it
        // is covered by the `argus-core` unit tests. This test is about the
        // synthesis itself, so the limit is turned off rather than waited out.
        // Plugin variables are namespaced `plugin.<plugin>.<name>` by the
        // variables host, and argus names its own keys `argus.*`.
        set_plugin_variable(&pool, "argus.summarize_min_interval", "0").await;

        let topic = seed_topic(&pool).await;
        let feed_a = seed_feed(&pool, topic, "https://a.test/feed.xml", None).await;
        let feed_b = seed_feed(&pool, topic, "https://b.test/feed.xml", None).await;

        // Two decide survivors, as the M1 pipeline would leave them.
        let mut articles = Vec::new();
        for (feed, url, title) in [
            (
                feed_a,
                "https://a.test/1",
                "Nvidia reports record datacenter revenue",
            ),
            (
                feed_b,
                "https://b.test/1",
                "Nvidia datacenter revenue hits a record",
            ),
        ] {
            let id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO argus_articles \
                    (id, url, title, content, published_at, feed_id, topic_id, \
                     relevance_score, pipeline_state, created, changed) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 85, 'decided', $8, $8)",
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
            .bind(now())
            .execute(&pool)
            .await
            .unwrap();
            insert_job(
                &pool,
                serde_json::json!({ "stage": "analyze", "id": id.to_string() }),
                5,
            )
            .await;
            articles.push(id);
        }

        let cron = cron_with_ai(pool.clone(), dispatcher());
        drain_until_idle(&cron, 14).await;

        // Analyze wrote all four prose fields and the raw model response.
        let rows: Vec<AnalysisColumns> = sqlx::query_as(
            "SELECT summary, critical_analysis, fallacy_analysis, source_analysis, analysis \
                 FROM argus_articles ORDER BY url",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        for (summary, critical, fallacy, source, raw) in &rows {
            assert!(
                summary.contains("who said so"),
                "the analyze summary: {summary}"
            );
            assert!(critical.as_deref().is_some_and(|c| c.contains("guidance")));
            assert!(fallacy.as_deref().is_some_and(|f| f.contains("authority")));
            assert!(source.as_deref().is_some_and(|s| s.contains("CFO")));
            assert!(raw.as_deref().is_some_and(|r| r.contains("\"entities\"")));
        }

        // Extract created one row per distinct entity, linked to both articles.
        let entities: Vec<(String, String, i32)> = sqlx::query_as(
            "SELECT canonical_name, entity_type, article_count FROM argus_entities \
             ORDER BY canonical_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        // Both titles name Nvidia and nothing else, so the second article
        // resolves onto the entity the first created rather than making a
        // second row — and the count reflects two articles, not two mentions.
        assert_eq!(
            entities
                .iter()
                .map(|(n, t, c)| (n.as_str(), t.as_str(), *c))
                .collect::<Vec<_>>(),
            vec![("Nvidia", "company", 2)],
            "one row per entity, counted once per article"
        );
        let links: i64 = sqlx::query_scalar("SELECT count(*) FROM argus_article_entities")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(links, 2);

        // Embed and cluster produced one story holding both.
        let (story_id, count): (Uuid, i32) =
            sqlx::query_as("SELECT id, article_count FROM argus_stories")
                .fetch_one(&pool)
                .await
                .expect("exactly one story");
        assert_eq!(count, 2);

        // Summarize wrote the narrative and the source list onto the Item.
        let (item_title, fields): (String, serde_json::Value) =
            sqlx::query_as("SELECT title, fields FROM item WHERE id = $1")
                .bind(story_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(item_title, "Nvidia posts a record datacenter quarter");
        assert!(
            fields["field_summary"]["value"]
                .as_str()
                .unwrap()
                .contains("disagree"),
            "the synthesized narrative, not the placeholder: {fields}"
        );
        let sources: Vec<serde_json::Value> =
            serde_json::from_str(fields["field_sources"]["value"].as_str().unwrap()).unwrap();
        assert_eq!(sources.len(), 2, "both reports credited as sources");
        assert!(fields["field_summary_updated"]["value"].as_i64().unwrap() > 0);

        // Cost was read from the response, per stage, priced.
        let spend: Vec<(String, i32, i32, f64)> = sqlx::query_as(
            "SELECT stage, calls, unpriced_calls, cost_usd FROM argus_cost_daily ORDER BY stage",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let by_stage: std::collections::HashMap<_, _> = spend
            .iter()
            .map(|(s, c, u, d)| (s.as_str(), (*c, *u, *d)))
            .collect();
        assert_eq!(
            by_stage.get("argus_analyze").map(|(c, u, _)| (*c, *u)),
            Some((2, 0)),
            "two priced analyze calls: {spend:?}"
        );
        // Two summarize calls, because the rate limit is off for this test: the
        // story is synthesized when it is founded and again when the second
        // report joins. With the default 10-minute limit the second would
        // coalesce into the first.
        assert_eq!(
            by_stage.get("argus_summarize").map(|(c, u, _)| (*c, *u)),
            Some((2, 0)),
            "priced summarize calls: {spend:?}"
        );
        let total: f64 = spend.iter().map(|(_, _, _, d)| d).sum();
        // 1000 input + 500 output tokens at the configured price = $0.002/call.
        assert!(
            (total - 0.008).abs() < 1e-9,
            "four calls at $0.002 each: {total}"
        );

        reset(&pool).await;
    });
}

// ── M2-12: the live smoke run against real public feeds ──────────────────────

/// Drive the whole pipeline against **real, public RSS feeds**, from
/// `tap_cron` through fetch, decide, analyze, extract, embed, cluster and
/// summarize, and print the numbers the milestone report quotes.
///
/// `#[ignore]` because it reaches the public internet: it is a manual run, not
/// a CI gate. Invoke it with
///
/// ```text
/// cargo test -p trovato-kernel --test argus_pipeline_test -- \
///     --ignored --nocapture real_feeds_smoke_run
/// ```
///
/// **What is real and what is not.** The feeds, the articles, the titles, the
/// bodies, the deduplication, every queue transition, every database write and
/// every clustering decision are real. The *model* is a local fixture: the
/// relevance score, the analysis and the synthesis are canned, because the AI
/// path needs a configured external provider and this run has none. Token
/// counts and therefore dollar costs are the fixture's, so the cost figures
/// below measure the accounting path end to end, not what a real provider
/// would charge. Nothing here is extrapolated from a run that did not happen.
#[test]
#[ignore = "reaches the public internet; run manually"]
fn real_feeds_smoke_run() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let base_url = start_fixture_provider().await;
        configure_fixture_provider(&pool, &base_url).await;

        let topic = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO argus_topics \
                (id, name, relevance_prompt, relevance_threshold, enabled, created, changed) \
             VALUES ($1, $2, 'Is this about technology, AI, or the technology industry?', \
                     40, true, $3, $3)",
        )
        .bind(topic)
        .bind(format!("smoke-{topic}"))
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();

        // Real, public, high-volume feeds (public IPs, so the SSRF fence
        // permits them).
        let feeds = [
            ("https://hnrss.org/frontpage", "Hacker News"),
            (
                "https://feeds.arstechnica.com/arstechnica/index",
                "Ars Technica",
            ),
            ("https://www.theverge.com/rss/index.xml", "The Verge"),
            (
                "https://feeds.bbci.co.uk/news/technology/rss.xml",
                "BBC Tech",
            ),
        ];
        for (url, name) in feeds {
            sqlx::query(
                "INSERT INTO argus_feeds \
                    (id, url, name, topic_id, fetch_interval_seconds, enabled, created, changed) \
                 VALUES (gen_random_uuid(), $1, $2, $3, 0, true, $4, $4)",
            )
            .bind(url)
            .bind(name)
            .bind(topic)
            .bind(now())
            .execute(&pool)
            .await
            .unwrap();
        }

        let disp = dispatcher();
        let state = RequestState::new(
            UserContext::background(),
            RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
                .with_plugin_runtime(disp.runtime().clone()),
        );
        disp.dispatch_to_plugin(
            "tap_cron",
            &serde_json::json!({ "timestamp": now() }).to_string(),
            PLUGIN,
            state,
        )
        .await
        .expect("argus implements tap_cron");

        let started = std::time::Instant::now();
        let cron = cron_with_ai(pool.clone(), dispatcher());
        let succeeded = drain_until_idle(&cron, 200).await;
        let elapsed = started.elapsed();

        // ---- the numbers -------------------------------------------------
        let states: Vec<(String, i64)> = sqlx::query_as(
            "SELECT pipeline_state, count(*) FROM argus_articles GROUP BY 1 ORDER BY 2 DESC",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM argus_articles")
            .fetch_one(&pool)
            .await
            .unwrap();
        let entities: i64 = sqlx::query_scalar("SELECT count(*) FROM argus_entities")
            .fetch_one(&pool)
            .await
            .unwrap();
        let stories: i64 = sqlx::query_scalar("SELECT count(*) FROM argus_stories")
            .fetch_one(&pool)
            .await
            .unwrap();
        let multi: i64 =
            sqlx::query_scalar("SELECT count(*) FROM argus_stories WHERE article_count > 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let dead: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM plugin_queue WHERE plugin_name = 'argus' AND status = 'dead'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let spend: Vec<(String, i32, i32, f64)> = sqlx::query_as(
            "SELECT stage, calls, unpriced_calls, cost_usd FROM argus_cost_daily ORDER BY stage",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let examples: Vec<(String, i32, i64)> = sqlx::query_as(
            "SELECT s.title, s.article_count, \
                    (SELECT count(DISTINCT a.feed_id) FROM argus_articles a WHERE a.story_id = s.id) \
             FROM argus_stories s WHERE s.article_count > 1 \
             ORDER BY s.article_count DESC LIMIT 5",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        let total_cost: f64 = spend.iter().map(|(_, _, _, c)| c).sum();
        let per_article = if total > 0 {
            total_cost / total as f64
        } else {
            0.0
        };

        println!("\n=== argus M2 live smoke run ===");
        println!("feeds:                {}", feeds.len());
        println!(
            "jobs drained:         {succeeded} in {:.1}s",
            elapsed.as_secs_f64()
        );
        println!("articles ingested:    {total}");
        for (state, n) in &states {
            println!("  {state:<12} {n}");
        }
        println!("entities extracted:   {entities}");
        println!("stories:              {stories} ({multi} with more than one article)");
        println!("dead-lettered jobs:   {dead}");
        println!("spend by stage:");
        for (stage, calls, unpriced, cost) in &spend {
            println!("  {stage:<18} calls={calls:<4} unpriced={unpriced:<4} usd={cost:.6}");
        }
        println!("total cost:           ${total_cost:.6}");
        println!("cost per article:     ${per_article:.6}");
        println!(
            "projected at 100 feeds (same articles-per-feed): ${:.4}",
            total_cost / feeds.len() as f64 * 100.0
        );
        println!("multi-source stories:");
        for (title, count, sources) in &examples {
            println!("  [{count} articles / {sources} sources] {title}");
        }
        println!("=== end ===\n");

        assert!(
            total > 0,
            "the run must ingest something to be worth reading"
        );
        reset(&pool).await;
    });
}

// ── M2: the AI-consuming stages behave without a provider ────────────────────

#[test]
fn analyze_without_provider_dead_letters() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;
        let topic = seed_topic(&pool).await;
        let article = seed_article(&pool, topic, "https://x.test/analyze", "decided").await;
        let job = insert_job(
            &pool,
            serde_json::json!({ "stage": "analyze", "id": article.to_string() }),
            1,
        )
        .await;

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();
        assert_eq!(stats.succeeded, 0, "analyze cannot run without a provider");

        let (status, _, dead_reason) = row_status(&pool, job).await.expect("job row exists");
        assert_eq!(status, "dead", "a poison analyze job dead-letters");
        assert!(dead_reason.is_some());
        // The article is untouched, so a configured provider re-analyzes it
        // cleanly rather than finding half a record.
        let state: String =
            sqlx::query_scalar("SELECT pipeline_state FROM argus_articles WHERE id = $1")
                .bind(article)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, "decided");
        let spend: i64 = sqlx::query_scalar("SELECT count(*) FROM argus_cost_daily")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(spend, 0, "a call that never happened costs nothing");

        reset(&pool).await;
    });
}
