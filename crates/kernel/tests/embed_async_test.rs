#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11f integration tests: **async auto-embed** (D-51, D-52) — the
//! kernel-internal producer, the native embed drain arm, coalescing, the
//! provider-failure → dead-letter path, backfill, and the observable
//! `item_embed_status`.
//!
//! These drive the real [`CronService::drain_embed_jobs`] arm and the real
//! `item_embed_status` / `plugin_queue` tables — no stubs. The environment need
//! not have pgvector: the embed *storage* half is pgvector-gated (and covered by
//! `vector_store_test`/`gather_semantic_test` where it exists), but every P11f
//! mechanic tested here — enqueue, coalescing, retry→DLQ, observable state,
//! backfill — is decoupled from pgvector by design so it is exercisable in CI
//! (which runs stock `postgres:16`).
//!
//! The drain claims by `plugin_name`, and these tests share the one reserved
//! `__kernel_embed` identity, so they are serialized on [`SERIAL`] and each
//! cleans the queue + status on entry. The provider-failure and backfill tests
//! also mutate `ai_providers`/`ai_defaults` `site_config`; they restore it
//! within the lock.

use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use sqlx::{PgPool, Row};
use uuid::Uuid;

use trovato_kernel::content::ItemService;
use trovato_kernel::cron::CronService;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, Item, UpdateItem};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::ai_provider::{
    AiOperationType, AiProviderConfig, AiProviderService, OperationModel, ProviderProtocol,
};
use trovato_kernel::services::embed_index::{self, EMBED_QUEUE_NAME, KERNEL_EMBED_PLUGIN};
use trovato_kernel::services::vector_store::PgVectorStore;
use trovato_kernel::tap::{RequestServices, TapDispatcher, TapRegistry, UserContext};

/// Serializes embed tests at the OS-thread level (shared `__kernel_embed` queue
/// identity + shared `ai_providers` config).
static SERIAL: Mutex<()> = Mutex::new(());

/// One shared multi-thread runtime so pool connections are never created on one
/// runtime and reused on another.
static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});

/// Run a test body serially on the shared runtime.
fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    RT.block_on(body);
}

/// Connect a pool to the test DB and ensure migrations are applied.
async fn fresh_pool() -> PgPool {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run migrations");
    pool
}

/// A cron service with the native embed drain wired (provider + vector store).
async fn embed_cron(pool: PgPool) -> CronService {
    let redis = redis::Client::open(trovato_test_utils::env::redis_url()).expect("redis client");
    let mut cron = CronService::new(redis, pool.clone());
    cron.set_ai_providers(std::sync::Arc::new(AiProviderService::new(pool.clone())));
    // Construct a pgvector store (self-reports availability; unavailable in CI).
    cron.set_vector_store(std::sync::Arc::new(PgVectorStore::new(pool).await));
    cron
}

/// One empty (no-plugin) dispatcher shared across the binary — a fresh
/// `PluginRuntime` reserves a large pooling-allocator slab, so build exactly one.
static EMPTY_DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

fn empty_dispatcher() -> Arc<TapDispatcher> {
    EMPTY_DISPATCHER
        .get_or_init(|| {
            let runtime =
                Arc::new(PluginRuntime::new(&PluginConfig::default()).expect("create runtime"));
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

/// A fully-wired `ItemService` (embedding provider + vector store), so its save
/// path runs the real P11f producer branch (enqueue, not inline embed).
async fn wired_items(pool: PgPool) -> ItemService {
    let ai = Arc::new(AiProviderService::new(pool.clone()));
    let store = Arc::new(PgVectorStore::new(pool.clone()).await);
    let tap_services = RequestServices::for_background(
        pool.clone(),
        Some(ai.clone()),
        None,
        reqwest::Client::new(),
    );
    ItemService::new(
        pool,
        empty_dispatcher(),
        tap_services,
        Duration::from_secs(1),
        Some(ai),
        Some(store),
        Default::default(),
    )
}

/// An admin context that bypasses item access (for the update path).
fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// Remove all kernel embed rows so a test starts from an empty queue.
async fn clean(pool: &PgPool) {
    sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
        .bind(KERNEL_EMBED_PLUGIN)
        .execute(pool)
        .await
        .unwrap();
}

/// Create a persisted item with a unique title and return it. Uses the
/// default-seeded `page` type and the nil author (both present from migrations).
async fn seed_item(pool: &PgPool, title: &str, body: &str) -> Item {
    let input = CreateItem {
        item_type: "page".to_string(),
        title: title.to_string(),
        author_id: Uuid::nil(),
        status: Some(1),
        promote: None,
        sticky: None,
        fields: Some(serde_json::json!({ "body": { "value": body } })),
        stage_id: Some(LIVE_STAGE_ID),
        language: None,
        log: None,
    };
    Item::create(pool, input).await.expect("create item")
}

/// Count kernel embed rows, optionally by status.
async fn count(pool: &PgPool, status: Option<&str>) -> i64 {
    match status {
        Some(s) => sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM plugin_queue WHERE plugin_name = $1 AND status = $2",
        )
        .bind(KERNEL_EMBED_PLUGIN)
        .bind(s),
        None => {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_queue WHERE plugin_name = $1")
                .bind(KERNEL_EMBED_PLUGIN)
        }
    }
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Insert a kernel embed job row directly (bypassing the producer) with an
/// explicit content hash and attempt budget; returns its id.
async fn insert_embed_job(
    pool: &PgPool,
    item_id: Uuid,
    content_hash: &str,
    max_attempts: i32,
) -> i64 {
    let payload = serde_json::json!({ "item_id": item_id, "content_hash": content_hash });
    let created = chrono::Utc::now().timestamp();
    sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, max_attempts,
             next_attempt_at, status, attempts, locked_until)
        VALUES ($1, $2, $3, $4, 0, $5, 0, 'ready', 0, 0)
        RETURNING id
        "#,
    )
    .bind(KERNEL_EMBED_PLUGIN)
    .bind(EMBED_QUEUE_NAME)
    .bind(&payload)
    .bind(created)
    .bind(max_attempts)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Write a resolvable-but-failing embedding provider (private base_url makes
/// `AiProviderService::embed` bail deterministically, offline, at its SSRF
/// `validate_base_url` guard). Returns the model string.
async fn set_failing_embed_provider(pool: &PgPool) -> String {
    let svc = AiProviderService::new(pool.clone());
    let config = AiProviderConfig {
        id: "p11f_test_embed".to_string(),
        label: "P11f failing embed".to_string(),
        protocol: ProviderProtocol::OpenAiCompatible,
        // Private IP: passes JSON parsing, fails the SSRF guard inside `embed`.
        base_url: "http://10.255.255.1".to_string(),
        api_key_env: String::new(),
        models: vec![OperationModel {
            operation: AiOperationType::Embedding,
            model: "test-embed-model".to_string(),
        }],
        rate_limit_rpm: 0,
        enabled: true,
    };
    svc.save_provider(config).await.unwrap();
    svc.set_default(AiOperationType::Embedding, "p11f_test_embed")
        .await
        .unwrap();
    "test-embed-model".to_string()
}

/// Restore `ai_providers`/`ai_defaults` to empty so config never leaks.
async fn clear_ai_config(pool: &PgPool) {
    let _ =
        trovato_kernel::models::SiteConfig::set(pool, "ai_providers", serde_json::json!([])).await;
    let _ =
        trovato_kernel::models::SiteConfig::set(pool, "ai_defaults", serde_json::json!({})).await;
}

// ── D-52: the kernel producer enqueues an embed job + marks it pending ───────

#[test]
fn enqueue_embed_job_records_queue_row_and_pending_state() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        let item = seed_item(&pool, "P11f enqueue test", "some body text").await;

        // The producer seam: enqueue + observable pending state. No provider
        // call happens here — this is exactly the save-path work.
        embed_index::enqueue_embed_job(&pool, item.id, "abc123")
            .await
            .unwrap();

        // A kernel embed job landed under the reserved identity + queue name.
        let row = sqlx::query(
            "SELECT queue_name, status, payload FROM plugin_queue \
             WHERE plugin_name = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(KERNEL_EMBED_PLUGIN)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("queue_name"), EMBED_QUEUE_NAME);
        assert_eq!(row.get::<String, _>("status"), "ready");
        let payload: serde_json::Value = row.get("payload");
        assert_eq!(payload["item_id"], serde_json::json!(item.id));

        // The item is observably `pending`.
        let status = embed_index::embed_status(&pool, item.id)
            .await
            .unwrap()
            .expect("status row");
        assert_eq!(status.state, "pending");
        assert_eq!(status.content_hash.as_deref(), Some("abc123"));

        clean(&pool).await;
    });
}

/// Count kernel embed jobs enqueued for a specific item.
async fn embed_jobs_for(pool: &PgPool, item_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM plugin_queue \
         WHERE plugin_name = $1 AND payload->>'item_id' = $2",
    )
    .bind(KERNEL_EMBED_PLUGIN)
    .bind(item_id.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

// ── D-52: the save path enqueues (both create AND update), never embeds inline ─

#[test]
fn save_create_and_update_both_enqueue_without_inline_embed() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        // A wired ItemService whose provider, if ever called inline, points at a
        // failing base_url — so an accidental synchronous embed would surface.
        clear_ai_config(&pool).await;
        set_failing_embed_provider(&pool).await;
        let items = wired_items(pool.clone()).await;

        // Create on the default (async) policy: enqueues, does not embed inline.
        let created = items
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    title: "P11f save-path create".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({ "body": { "value": "first" } })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: None,
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("create");

        assert_eq!(
            embed_jobs_for(&pool, created.id).await,
            1,
            "create must enqueue exactly one embed job"
        );
        let st = embed_index::embed_status(&pool, created.id).await.unwrap();
        assert_eq!(st.map(|s| s.state), Some("pending".to_string()));

        // The update call site must ALSO enqueue — otherwise every edit silently
        // re-imposes the synchronous embed tax the async path removed.
        items
            .update(
                created.id,
                UpdateItem {
                    author_id: None,
                    created: None,
                    stage_id: None,
                    title: Some("P11f save-path update".to_string()),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({ "body": { "value": "second" } })),
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("update")
            .expect("item exists");

        assert_eq!(
            embed_jobs_for(&pool, created.id).await,
            2,
            "update must enqueue a second embed job (both call sites)"
        );

        clean(&pool).await;
        clear_ai_config(&pool).await;
    });
}

// ── D-52 coalescing: a superseded job is skipped, not re-embedded ────────────

#[test]
fn superseded_job_is_coalesced_away_on_drain() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        // Item currently holds "version two"; a stale job captured the hash of
        // "version one" (an earlier save the newer save superseded).
        let item = seed_item(&pool, "P11f coalesce test", "version two").await;
        let stale_hash = embed_index::embed_content_hash("stale version one text");
        let id = insert_embed_job(&pool, item.id, &stale_hash, 5).await;

        let stats = embed_cron(pool.clone())
            .await
            .drain_embed_jobs()
            .await
            .unwrap();

        // The stale job is consumed as a success (coalesced away), never
        // retried or dead-lettered — the newer content's job embeds once.
        assert_eq!(stats.dead_lettered, 0);
        assert_eq!(stats.retried, 0);
        assert_eq!(count(&pool, None).await, 0, "stale job should be deleted");
        let gone = sqlx::query("SELECT id FROM plugin_queue WHERE id = $1")
            .bind(id)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(gone.is_none());

        clean(&pool).await;
    });
}

// ── D-51/D-52: provider failure → retry → DLQ + observable `failed` state ────

#[test]
fn provider_failure_dead_letters_and_marks_item_failed() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        clear_ai_config(&pool).await;
        set_failing_embed_provider(&pool).await;

        let item = seed_item(&pool, "P11f DLQ test", "body that will fail to embed").await;
        // max_attempts = 1: the claim bumps attempts to 1, so the first failure
        // (attempts >= max_attempts) dead-letters immediately — no backoff wait.
        let current = embed_index::item_content_hash(&item);
        let id = insert_embed_job(&pool, item.id, &current, 1).await;

        let stats = embed_cron(pool.clone())
            .await
            .drain_embed_jobs()
            .await
            .unwrap();
        assert_eq!(stats.dead_lettered, 1, "provider failure must dead-letter");

        // The row is dead with the provider error preserved.
        let row =
            sqlx::query("SELECT status, dead_reason, last_error FROM plugin_queue WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.get::<String, _>("status"), "dead");
        let reason: String = row.get("dead_reason");
        assert!(
            reason.contains("embed provider failed"),
            "dead_reason should carry the provider error, got: {reason}"
        );

        // The item's observable state flips to `failed` (no more silent drop).
        let status = embed_index::embed_status(&pool, item.id)
            .await
            .unwrap()
            .expect("status row");
        assert_eq!(status.state, "failed");
        assert!(status.last_error.is_some());

        // The item itself is unharmed and still loads (text search is
        // unaffected — the `search_vector` trigger ran in the save txn).
        let reloaded = Item::find_by_id(&pool, item.id).await.unwrap();
        assert!(reloaded.is_some(), "item must survive a failed embed");

        clean(&pool).await;
        clear_ai_config(&pool).await;
    });
}

// ── Backfill: gaps get bounded-batch embed jobs ──────────────────────────────

#[test]
fn backfill_enqueues_jobs_for_gap_items() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        clear_ai_config(&pool).await;
        let model = set_failing_embed_provider(&pool).await;

        // Three fresh items with no embedding status — all gaps for the model.
        let a = seed_item(&pool, "P11f backfill A", "alpha").await;
        let b = seed_item(&pool, "P11f backfill B", "bravo").await;
        let c = seed_item(&pool, "P11f backfill C", "charlie").await;

        let before = count(&pool, None).await;
        let enqueued = embed_index::enqueue_backfill(&pool, &model, 100)
            .await
            .unwrap();
        assert!(enqueued >= 3, "at least the three seeded gaps enqueue");
        let after = count(&pool, None).await;
        assert_eq!(
            after - before,
            enqueued as i64,
            "one queue row per backfill"
        );

        // Each seeded item is now observably pending.
        for id in [a.id, b.id, c.id] {
            let s = embed_index::embed_status(&pool, id).await.unwrap();
            assert_eq!(s.map(|s| s.state), Some("pending".to_string()));
        }

        clean(&pool).await;
        clear_ai_config(&pool).await;
    });
}

// ── B188: deleting an item drops its queued embed job ────────────────────────

#[test]
fn deleting_an_item_removes_its_queued_embed_jobs() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        clear_ai_config(&pool).await;
        set_failing_embed_provider(&pool).await;
        let items = wired_items(pool.clone()).await;

        // Save on the default (async) policy: one embed job is queued.
        let created = items
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    title: "B188 delete drops embed job".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({ "body": { "value": "doomed" } })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: None,
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("create");
        assert_eq!(embed_jobs_for(&pool, created.id).await, 1);

        // Another item's embed job and another plugin's row must survive.
        let other = seed_item(&pool, "B188 bystander", "stays").await;
        let other_job = insert_embed_job(&pool, other.id, "h", 5).await;
        let payload = serde_json::json!({ "item_id": created.id });
        let foreign_job = sqlx::query_scalar::<_, i64>(
            "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at) \
             VALUES ('b188_other_plugin', 'q', $1, 0) RETURNING id",
        )
        .bind(&payload)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert!(items.delete(created.id, &admin()).await.expect("delete"));

        assert_eq!(
            embed_jobs_for(&pool, created.id).await,
            0,
            "deleting an item must leave no __kernel_embed row for it"
        );
        let survivors =
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_queue WHERE id = ANY($1)")
                .bind(vec![other_job, foreign_job])
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            survivors, 2,
            "other items' and other plugins' rows are untouched"
        );

        sqlx::query("DELETE FROM plugin_queue WHERE id = $1")
            .bind(foreign_job)
            .execute(&pool)
            .await
            .unwrap();
        let _ = Item::delete(&pool, other.id).await;
        clean(&pool).await;
        clear_ai_config(&pool).await;
    });
}

#[test]
fn drain_drops_a_job_whose_item_is_gone() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        // A job left behind for an item that no longer exists (e.g. queued
        // before the delete-path fix, or raced with a delete).
        let missing = Uuid::new_v4();
        let id = insert_embed_job(&pool, missing, "whatever", 5).await;

        let logs = captured_logs();
        let cron = embed_cron(pool.clone()).await;
        let before = logs.contents().len();
        let stats = cron.drain_embed_jobs().await.unwrap();
        let text = logs.contents()[before..].to_string();

        // Quiet: nothing above debug from the kernel, and at most one debug line
        // from the drain.
        let kernel: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("trovato_kernel"))
            .collect();
        assert!(
            kernel
                .iter()
                .all(|l| l.contains("DEBUG") || l.contains("TRACE")),
            "a job for a missing item must not log above debug:\n{text}"
        );
        let drain_debug = kernel
            .iter()
            .filter(|l| l.contains("DEBUG") && l.contains("trovato_kernel::cron"))
            .count();
        assert!(
            drain_debug <= 1,
            "at most one debug line for a job whose item is gone, got {drain_debug}:\n{text}"
        );

        // Treated as done: removed, never retried or dead-lettered, no error.
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.retried, 0);
        assert_eq!(stats.dead_lettered, 0);
        let gone = sqlx::query("SELECT id FROM plugin_queue WHERE id = $1")
            .bind(id)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(gone.is_none(), "a job for a missing item must be removed");
        assert!(
            embed_index::embed_status(&pool, missing)
                .await
                .unwrap()
                .is_none()
        );

        clean(&pool).await;
    });
}

/// Shared buffer the test binary's `tracing` subscriber writes into, so a test
/// can read what the drain actually logged.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The capturing subscriber, installed once for the whole binary. Global, not
/// thread-local: the drain runs each job in a spawned task on a runtime worker
/// thread, which `set_default` would not reach.
static CAPTURE: OnceLock<CapturedLogs> = OnceLock::new();

fn captured_logs() -> &'static CapturedLogs {
    CAPTURE.get_or_init(|| {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("install the capturing subscriber");
        logs
    })
}

/// How long a waiter is given to show it is blocked on the other transaction's
/// row lock before that transaction commits.
const LOCK_WAIT: Duration = Duration::from_millis(300);

#[test]
fn an_enqueue_racing_an_uncommitted_delete_queues_nothing() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        let item = seed_item(&pool, "B188 enqueue during delete", "racing").await;

        // The delete has run but not committed.
        let mut tx = pool.begin().await.unwrap();
        assert!(Item::delete_in(&mut tx, item.id).await.unwrap());

        // A save's enqueue for the same item arrives meanwhile.
        let enqueue = tokio::spawn({
            let pool = pool.clone();
            let id = item.id;
            async move { embed_index::enqueue_embed_job(&pool, id, "h").await }
        });
        tokio::time::sleep(LOCK_WAIT).await;
        assert!(
            !enqueue.is_finished(),
            "the enqueue must wait on the item row lock held by the delete"
        );

        tx.commit().await.unwrap();
        let queued = enqueue
            .await
            .unwrap()
            .expect("enqueue for a deleted item is not an error");
        assert!(!queued, "nothing is queued for an item deleted meanwhile");
        assert_eq!(
            embed_jobs_for(&pool, item.id).await,
            0,
            "no embed job may outlive the item"
        );

        clean(&pool).await;
    });
}

#[test]
fn a_delete_racing_an_uncommitted_enqueue_removes_its_job() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        let item = seed_item(&pool, "B188 delete during enqueue", "racing").await;

        // An enqueue that has locked the item and written its job, not yet
        // committed: the same steps `enqueue_embed_job` takes.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM item WHERE id = $1 FOR KEY SHARE")
            .bind(item.id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let payload = serde_json::json!({ "item_id": item.id, "content_hash": "h" });
        sqlx::query(
            "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at) \
             VALUES ($1, $2, $3, 0)",
        )
        .bind(KERNEL_EMBED_PLUGIN)
        .bind(EMBED_QUEUE_NAME)
        .bind(&payload)
        .execute(&mut *tx)
        .await
        .unwrap();

        // The delete arrives meanwhile.
        let delete = tokio::spawn({
            let pool = pool.clone();
            let id = item.id;
            async move { Item::delete(&pool, id).await }
        });
        tokio::time::sleep(LOCK_WAIT).await;
        assert!(
            !delete.is_finished(),
            "the delete must wait on the enqueue's lock on the item row"
        );

        tx.commit().await.unwrap();
        assert!(
            delete.await.unwrap().expect("delete"),
            "the item is deleted"
        );
        assert_eq!(
            embed_jobs_for(&pool, item.id).await,
            0,
            "the job committed while the delete waited must go with the item"
        );

        clean(&pool).await;
    });
}

/// The embed drain stops claiming once its time budget is spent, as the plugin
/// drain does.
///
/// It checked only the item cap, so a slow provider kept it claiming fresh
/// batches until the cron run's outer bound cut it off, and every job in the
/// batch it held then lost an attempt to work that was never allowed to
/// finish. With the budget already spent, nothing may be claimed at all.
#[test]
fn the_embed_drain_claims_nothing_once_its_budget_is_spent() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        let id = insert_embed_job(&pool, Uuid::new_v4(), "whatever", 5).await;

        let mut cron = embed_cron(pool.clone()).await;
        cron.set_drain_budget(Duration::ZERO);
        let stats = cron.drain_embed_jobs().await.unwrap();

        let attempts: Option<i32> =
            sqlx::query_scalar("SELECT attempts FROM plugin_queue WHERE id = $1")
                .bind(id)
                .fetch_optional(&pool)
                .await
                .unwrap();
        clean(&pool).await;

        assert_eq!(stats.total(), 0, "a drain with no budget left ran jobs");
        assert_eq!(
            attempts,
            Some(0),
            "the job must still be waiting, unclaimed, with no attempt spent"
        );
    });
}
