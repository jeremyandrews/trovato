#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11d integration tests: **plugin queue v2** (D-45..D-48).
//!
//! These drive the real `test_queue_worker` fixture through the real
//! [`TapDispatcher`] and the real [`CronService`] drain — not stubs — so they
//! exercise claim-locking, backoff/retry accounting, dead-lettering, honored
//! concurrency, per-plugin fairness, and the additive `enqueue` host function
//! end to end.
//!
//! Build the fixture first:
//! `cargo build -p test_queue_worker --target wasm32-wasip1 --release \
//!   && cp target/wasm32-wasip1/release/test_queue_worker.wasm plugins/test_queue_worker/`
//!
//! The drain claims by `plugin_name`, so all tests here share the one
//! `test_queue_worker` plugin and would steal each other's rows if run
//! concurrently. They are serialized on [`SERIAL`] and run on one shared
//! runtime ([`RT`]) — a std `Mutex` held across `block_on` gives OS-thread-level
//! serialization that a cross-runtime async mutex cannot — and each test cleans
//! the queue on entry.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::PgPool;
use sqlx::Row;

use trovato_kernel::cron::CronService;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

/// The fixture plugin name (also its `plugin_queue.plugin_name`).
const FIXTURE: &str = "test_queue_worker";

/// A second real plugin that exports `tap_queue_worker`, used to prove the
/// per-plugin cap is measured per-plugin. `argus` returns an error VALUE (not a
/// trap) for an unrecognized payload, and queue v2 counts a non-trapping return
/// as success — so its jobs land in `QueueDrainStats::succeeded` alongside the
/// fixture's, which is exactly the contamination under test.
const SECOND_WORKER: &str = "argus";

/// Serializes queue tests at the OS-thread level (see module docs).
static SERIAL: Mutex<()> = Mutex::new(());

/// One shared multi-thread runtime for all queue tests, so pool connections are
/// never created on one runtime and reused on another.
static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});

/// Run a test body serially on the shared runtime. The std guard is held for the
/// whole (blocking) `block_on`, so concurrent test threads wait at the OS level.
fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    RT.block_on(body);
}

/// One shared fixture dispatcher for the whole test binary — a fresh
/// `PluginRuntime` reserves a large pooling-allocator address slab, so we build
/// exactly one and reuse it (it is `Send + Sync` and runtime-agnostic).
static FIXTURE_DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

/// Repo `plugins/` directory (two levels up from this crate).
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// Build (once) a dispatcher with only the queue-v2 fixture loaded.
fn dispatcher() -> Arc<TapDispatcher> {
    FIXTURE_DISPATCHER
        .get_or_init(|| {
            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
            runtime
                .load_plugin(&plugins_dir().join(FIXTURE))
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to load fixture '{FIXTURE}': {e:#}\n\
                         build it: cargo build -p {FIXTURE} --target wasm32-wasip1 --release \
                         && cp target/wasm32-wasip1/release/{FIXTURE}.wasm plugins/{FIXTURE}/"
                    )
                });
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

/// Connect a pool to the test DB and ensure migrations are applied (idempotent).
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

/// Build a `CronService` wired to the fixture dispatcher on `pool`.
fn cron_with(pool: PgPool, disp: Arc<TapDispatcher>) -> Arc<CronService> {
    let redis = redis::Client::open("redis://127.0.0.1:6379").expect("redis client");
    let mut cron = CronService::new(redis, pool);
    cron.set_tap_dispatcher(disp);
    Arc::new(cron)
}

/// Remove all fixture rows so a test starts from an empty queue.
async fn clean_queue(pool: &PgPool) {
    sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
        .bind(FIXTURE)
        .execute(pool)
        .await
        .unwrap();
}

/// Insert one job with explicit v2 fields; returns its id.
async fn insert_job(
    pool: &PgPool,
    payload: serde_json::Value,
    priority: i32,
    max_attempts: i32,
    created_at: i64,
) -> i64 {
    let row = sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, max_attempts,
             next_attempt_at, status, attempts, locked_until)
        VALUES ($1, 'test_queue', $2, $3, $4, $5, 0, 'ready', 0, 0)
        RETURNING id
        "#,
    )
    .bind(FIXTURE)
    .bind(&payload)
    .bind(created_at)
    .bind(priority)
    .bind(max_attempts)
    .fetch_one(pool)
    .await
    .unwrap();
    row.get::<i64, _>("id")
}

/// Count rows for the fixture, optionally filtered by status.
async fn count(pool: &PgPool, status: Option<&str>) -> i64 {
    match status {
        Some(s) => sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM plugin_queue WHERE plugin_name = $1 AND status = $2",
        )
        .bind(FIXTURE)
        .bind(s)
        .fetch_one(pool)
        .await
        .unwrap(),
        None => {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_queue WHERE plugin_name = $1")
                .bind(FIXTURE)
                .fetch_one(pool)
                .await
                .unwrap()
        }
    }
}

/// Fetch one row's v2 bookkeeping columns.
async fn row_state(pool: &PgPool, id: i64) -> (String, i32, i64, Option<String>, Option<String>) {
    let row = sqlx::query(
        "SELECT status, attempts, next_attempt_at, last_error, dead_reason
         FROM plugin_queue WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        row.get("status"),
        row.get("attempts"),
        row.get("next_attempt_at"),
        row.get("last_error"),
        row.get("dead_reason"),
    )
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

// ── D-45: schema survives v1 rows ────────────────────────────────────────────

#[test]
fn v1_style_row_defaults_to_ready() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // Insert exactly the v1 column set (no v2 columns) — the migration's
        // defaults must make it a ready, un-attempted job so in-flight rows survive.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at)
         VALUES ($1, 'test_queue', '{\"outcome\":\"ok\"}'::jsonb, $2) RETURNING id",
        )
        .bind(FIXTURE)
        .bind(now())
        .fetch_one(&pool)
        .await
        .unwrap();

        let (status, attempts, next_at, last_error, dead_reason) = row_state(&pool, id).await;
        assert_eq!(status, "ready");
        assert_eq!(attempts, 0);
        assert_eq!(next_at, 0);
        assert!(last_error.is_none());
        assert!(dead_reason.is_none());

        clean_queue(&pool).await;
    });
}

// ── D-48: additive enqueue carries priority + delay ──────────────────────────

#[test]
fn enqueue_host_fn_sets_priority_and_delay() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // The fixture's tap_cron enqueues two jobs via the real `enqueue` host fn:
        // one priority 10 / no delay, one priority 0 / delay 3600.
        let disp = dispatcher();
        let state = RequestState::new(
            UserContext::background(),
            RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
                .with_plugin_runtime(disp.runtime().clone()),
        );
        let before = now();
        disp.dispatch_to_plugin("tap_cron", "{}", FIXTURE, state)
            .await
            .expect("fixture implements tap_cron");

        let rows = sqlx::query(
            "SELECT priority, next_attempt_at, payload FROM plugin_queue
         WHERE plugin_name = $1 ORDER BY priority DESC",
        )
        .bind(FIXTURE)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2, "tap_cron should enqueue two jobs");

        // High-priority job: priority 10, eligible ~immediately.
        let p0: i32 = rows[0].get("priority");
        let n0: i64 = rows[0].get("next_attempt_at");
        assert_eq!(p0, 10);
        assert!(
            (before..=now() + 2).contains(&n0),
            "no-delay job eligible now"
        );

        // Delayed job: priority 0, deferred ~1 hour.
        let p1: i32 = rows[1].get("priority");
        let n1: i64 = rows[1].get("next_attempt_at");
        assert_eq!(p1, 0);
        assert!(n1 >= before + 3600, "delayed job deferred by ~3600s: {n1}");

        clean_queue(&pool).await;
    });
}

// ── D-47: claim under contention, no double-delivery (SKIP LOCKED) ────────────

#[test]
fn concurrent_drains_deliver_each_job_exactly_once() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        const N: usize = 24;
        for i in 0..N {
            insert_job(
                &pool,
                serde_json::json!({"outcome": "ok", "i": i}),
                0,
                5,
                now(),
            )
            .await;
        }

        // Four drainers race on the same queue. FOR UPDATE SKIP LOCKED must give
        // each a disjoint claim set, so the successes sum to exactly N (a
        // double-delivery would count a job's success twice).
        let cron = cron_with(pool.clone(), dispatcher());
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let c = cron.clone();
            set.spawn(async move { c.drain_plugin_queues().await.unwrap() });
        }
        let mut total_succeeded = 0u64;
        while let Some(joined) = set.join_next().await {
            total_succeeded += joined.unwrap().succeeded;
        }

        assert_eq!(total_succeeded, N as u64, "each job delivered exactly once");
        assert_eq!(count(&pool, None).await, 0, "all jobs consumed");

        clean_queue(&pool).await;
    });
}

/// A dispatcher loading the fixture **and** a second `tap_queue_worker` plugin.
///
/// Used only by `cap_is_measured_per_plugin_not_across_plugins`. Built fresh
/// rather than shared, because the point is to have a drain that visits two
/// plugins — which the shared single-fixture dispatcher deliberately cannot.
fn dispatcher_with_second_worker() -> Arc<TapDispatcher> {
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&plugins_dir().join(FIXTURE))
        .expect("load queue fixture");
    runtime
        .load_plugin(&plugins_dir().join(SECOND_WORKER))
        .unwrap_or_else(|e| {
            panic!(
                "failed to load '{SECOND_WORKER}': {e:#}\n\
                 build it: cargo build -p {SECOND_WORKER} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{SECOND_WORKER}.wasm plugins/{SECOND_WORKER}/"
            )
        });
    let runtime = Arc::new(runtime);
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    Arc::new(TapDispatcher::new(runtime, registry))
}

/// Insert a ready job for an arbitrary plugin (not just the fixture).
async fn insert_job_for(pool: &PgPool, plugin: &str, payload: serde_json::Value) -> i64 {
    let row = sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, max_attempts,
             next_attempt_at, status, attempts, locked_until)
        VALUES ($1, 'test_queue', $2, $3, 0, 5, 0, 'ready', 0, 0)
        RETURNING id
        "#,
    )
    .bind(plugin)
    .bind(&payload)
    .bind(now())
    .fetch_one(pool)
    .await
    .unwrap();
    row.get::<i64, _>("id")
}

/// Count rows for an arbitrary plugin.
async fn count_for(pool: &PgPool, plugin: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_queue WHERE plugin_name = $1")
        .bind(plugin)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Classify an over-cap observation for the two cap-bounded tests.
///
/// Sharpened 2026-07-27 after the 2026-07-21 trap fired (109/114/120 of 120) and
/// its label sent the reader to the wrong place. Two corrections:
///
/// 1. **"claim overshoot in the drain loop" is not a possible cause.** The drain
///    holds `while processed < MAX_QUEUE_ITEMS_PER_CYCLE` and computes
///    `batch = width.min(MAX - processed)`, and `claim_batch` applies that as a
///    SQL `LIMIT` over a unique primary key. So `processed` cannot exceed the
///    cap within one `drain_plugin_queues` call, whatever the timing. A recorded
///    count above 100 therefore means MORE THAN ONE drain contributed, or the
///    starting row count was not what the test assumed — not that the loop
///    arithmetic leaked.
///
/// 2. **The cap is PER PLUGIN; `QueueDrainStats` is not.** `drain_plugin_queues`
///    accumulates one stats struct across every plugin it visits, so
///    `stats.succeeded` is a cross-plugin total. Reading it as this plugin's
///    contribution trips with nothing wrong the moment a second
///    `tap_queue_worker` plugin has ready jobs — demonstrated by
///    `cap_is_measured_per_plugin_not_across_plugins` (105 vs 100). Callers now
///    pass the measured per-plugin `consumed`, and `succeeded` is reported only
///    as diagnostic context.
///
/// 3. **`consumed` is only meaningful if the queue really started at `seeded`.**
///    The original trap hard-coded that assumption. This suite shares one
///    database with every other test binary, and a contaminated starting count
///    silently turns `consumed` into a fiction — exactly the class of bug that
///    was found and fixed in the shared conference seeder the same day. The
///    caller now measures the precondition and passes it in.
fn classify_over_cap(
    seeded_observed: i64,
    seeded_expected: i64,
    consumed: i64,
    succeeded: u64,
    total: i64,
    ready: i64,
    claimed: i64,
) -> String {
    if seeded_observed != seeded_expected {
        return format!(
            "PRECONDITION VIOLATED: queue held {seeded_observed} rows before the drain,              expected {seeded_expected}. Something outside this test wrote to              plugin_queue for the fixture plugin, so `consumed` arithmetic is              meaningless here. Fix the contamination first; this is NOT evidence              about the cap."
        );
    }
    let base = format!(
        "recorded succeeded={succeeded} vs consumed={consumed}          (started={seeded_observed}, remaining total={total}, ready={ready}, claimed={claimed})"
    );
    if succeeded as i64 > consumed {
        format!(
            "{base} -> recorded > consumed: DOUBLE-COUNT / DOUBLE-DELIVERY.              A job's outcome was recorded twice (suspect the JoinSet record path)              or a lease-expiry reclaim re-ran a job inside one drain."
        )
    } else if succeeded > 100 {
        format!(
            "{base} -> recorded == consumed > 100: MORE THAN ONE DRAIN CONTRIBUTED.              A single drain provably cannot exceed the cap (see this function's              docs), so look for a concurrent or leaked drainer against the same              plugin -- NOT for claim overshoot in the drain loop."
        )
    } else {
        format!(
            "{base} -> recorded < 100: the drain stopped SHORT of the cap.              Suspect rows becoming unclaimable mid-drain (lease/backoff), or a              competing claimer taking rows this drain expected."
        )
    }
}

// ── The cap is PER PLUGIN; the stats it is read from are NOT ─────────────────

/// `drain_plugin_queues` accumulates ONE `QueueDrainStats` across every plugin
/// it visits, while `MAX_QUEUE_ITEMS_PER_CYCLE` is applied per plugin. So
/// `stats.succeeded` is an aggregate and is NOT a valid proxy for "this plugin's
/// contribution" — asserting `stats.succeeded == 100` reads a cross-plugin total
/// against a per-plugin bound and trips with nothing wrong.
///
/// The shared fixture dispatcher happens to load exactly one `tap_queue_worker`
/// plugin, and the drain skips plugins it has no handler for, so the two numbers
/// coincide there by accident rather than by design. This test removes that
/// accident: it loads a second real worker, gives it ready jobs, and pins the
/// distinction so the aggregate can never quietly become the assertion again.
#[test]
fn cap_is_measured_per_plugin_not_across_plugins() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;
        sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
            .bind(SECOND_WORKER)
            .execute(&pool)
            .await
            .unwrap();

        // Over the cap for the fixture, plus a handful for the second worker.
        for i in 0..120 {
            insert_job(
                &pool,
                serde_json::json!({"outcome": "ok", "i": i}),
                0,
                5,
                now(),
            )
            .await;
        }
        const SECOND_JOBS: i64 = 5;
        for i in 0..SECOND_JOBS {
            // Unrecognized payload: argus returns an error value rather than
            // trapping, which queue v2 records as a success.
            insert_job_for(&pool, SECOND_WORKER, serde_json::json!({"not_a_stage": i})).await;
        }

        let fixture_before = count(&pool, None).await;
        let second_before = count_for(&pool, SECOND_WORKER).await;
        assert_eq!(fixture_before, 120);
        assert_eq!(second_before, SECOND_JOBS);

        let cron = cron_with(pool.clone(), dispatcher_with_second_worker());
        let stats = cron.drain_plugin_queues().await.unwrap();

        let fixture_consumed = fixture_before - count(&pool, None).await;
        let second_consumed = second_before - count_for(&pool, SECOND_WORKER).await;

        // The invariant that actually matters: THIS plugin was capped at 100.
        assert_eq!(
            fixture_consumed, 100,
            "the per-plugin cap must bound this plugin's own consumption"
        );

        // The second plugin drained independently, under its own cap.
        assert_eq!(
            second_consumed, SECOND_JOBS,
            "the second worker's jobs drain under their own per-plugin budget"
        );

        // And the aggregate is therefore ABOVE 100 with nothing wrong — which is
        // precisely why the cap-bounded tests must not assert on it.
        assert!(
            stats.succeeded > 100,
            "expected the cross-plugin aggregate to exceed the per-plugin cap \
             (fixture={fixture_consumed}, second={second_consumed}, \
             aggregate succeeded={}) — if this ever fails, the drain stopped \
             aggregating and the cap tests can go back to reading stats directly",
            stats.succeeded
        );

        clean_queue(&pool).await;
        sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
            .bind(SECOND_WORKER)
            .execute(&pool)
            .await
            .unwrap();
    });
}

// ── D-46: retry with backoff, then dead-letter at max_attempts ────────────────

#[test]
fn failing_job_retries_with_backoff_then_dead_letters() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // A poison job with max_attempts = 2.
        let id = insert_job(&pool, serde_json::json!({"outcome": "trap"}), 0, 2, now()).await;
        let cron = cron_with(pool.clone(), dispatcher());

        // First drain: worker traps → attempt 1, rescheduled with backoff (future),
        // error preserved, still ready.
        let s1 = cron.drain_plugin_queues().await.unwrap();
        assert_eq!(s1.retried, 1);
        assert_eq!(s1.dead_lettered, 0);
        let (status, attempts, next_at, last_error, _) = row_state(&pool, id).await;
        assert_eq!(status, "ready");
        assert_eq!(attempts, 1);
        assert!(next_at > now(), "rescheduled into the future (backoff)");
        assert!(last_error.is_some(), "error preserved across retry");

        // Simulate the backoff window elapsing.
        sqlx::query("UPDATE plugin_queue SET next_attempt_at = 0 WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        // Second drain: attempt 2 == max_attempts → dead-lettered with reason.
        let s2 = cron.drain_plugin_queues().await.unwrap();
        assert_eq!(s2.retried, 0);
        assert_eq!(s2.dead_lettered, 1);
        let (status, attempts, _, last_error, dead_reason) = row_state(&pool, id).await;
        assert_eq!(status, "dead");
        assert_eq!(attempts, 2);
        assert!(last_error.is_some());
        assert!(dead_reason.is_some(), "dead-letter reason recorded");

        // Nothing left ready — it never retries forever.
        assert_eq!(count(&pool, Some("ready")).await, 0);

        clean_queue(&pool).await;
    });
}

// ── Poison isolation: a permanently-failing job does not block its queue ──────

#[test]
fn poison_job_does_not_block_other_jobs() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // A poison job at the HEAD of the queue (oldest created_at, max_attempts 1)
        // plus two good jobs behind it. Under v1 the poison blocked the queue
        // forever; v2 must dead-letter it and drain the good jobs in the same pass.
        let base = now() - 100;
        let poison = insert_job(&pool, serde_json::json!({"outcome": "trap"}), 0, 1, base).await;
        insert_job(
            &pool,
            serde_json::json!({"outcome": "ok", "n": 1}),
            0,
            5,
            base + 1,
        )
        .await;
        insert_job(
            &pool,
            serde_json::json!({"outcome": "ok", "n": 2}),
            0,
            5,
            base + 2,
        )
        .await;

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        assert_eq!(stats.succeeded, 2, "both good jobs processed");
        assert_eq!(stats.dead_lettered, 1, "poison dead-lettered");
        assert_eq!(
            count(&pool, Some("ready")).await,
            0,
            "queue advanced past poison"
        );
        let (status, _, _, _, _) = row_state(&pool, poison).await;
        assert_eq!(status, "dead");

        clean_queue(&pool).await;
    });
}

// ── Error-shaped worker result is a SUCCESS, not a retry (ritrovo parity) ─────

#[test]
fn worker_error_result_is_deleted_not_retried() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // The fixture returns `{"status":"error"}` for outcome "error" — a positive-
        // length (successful) dispatch. The drain must delete it, exactly as the
        // reference importer's `{"status":"error"}` returns behave today; only a
        // trap counts as a failed attempt.
        insert_job(&pool, serde_json::json!({"outcome": "error"}), 0, 5, now()).await;
        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        assert_eq!(stats.succeeded, 1);
        assert_eq!(stats.retried, 0);
        assert_eq!(stats.dead_lettered, 0);
        assert_eq!(count(&pool, None).await, 0, "error-result job deleted");

        clean_queue(&pool).await;
    });
}

// ── Concurrency: a full width-4 batch is claimed and dispatched together ──────

#[test]
fn width_bounded_batch_processes_in_one_pass() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // The fixture declares concurrency 8; the kernel clamps to 4. Four jobs are
        // claimed as one width-4 batch and dispatched concurrently.
        for i in 0..4 {
            insert_job(
                &pool,
                serde_json::json!({"outcome": "ok", "i": i}),
                0,
                5,
                now(),
            )
            .await;
        }
        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        assert_eq!(stats.succeeded, 4);
        assert_eq!(count(&pool, None).await, 0);

        clean_queue(&pool).await;
    });
}

// ── Fairness: the per-plugin per-cycle cap bounds a flood ─────────────────────

#[test]
fn per_plugin_cap_bounds_a_flood() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // 120 jobs from one plugin; the drain processes at most
        // MAX_QUEUE_ITEMS_PER_CYCLE (100) per plugin per cycle, so a flood cannot
        // consume unbounded work in a single pass — this is the per-plugin fairness
        // mechanism (cross-plugin isolation follows structurally: the drain loops
        // plugins independently and applies the cap to each).
        for i in 0..120 {
            insert_job(
                &pool,
                serde_json::json!({"outcome": "ok", "i": i}),
                0,
                5,
                now(),
            )
            .await;
        }
        // Measure the precondition rather than assuming it. `consumed` is
        // computed as `seeded - remaining`, which is a fiction if anything else
        // wrote to this plugin's queue; capturing the real starting count lets
        // the trap tell "the cap broke" apart from "the fixture was polluted".
        let seeded = count(&pool, None).await;

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        // Over-cap trap (P11d). A single drain of one plugin provably caps at
        // MAX_QUEUE_ITEMS_PER_CYCLE (100): the drain loop holds
        // `processed < MAX_QUEUE_ITEMS_PER_CYCLE` and `claim_batch` LIMITs every
        // batch, no background tick runs in this harness, and `clean_queue`
        // full-deletes this plugin's rows. So `succeeded != 100` is impossible
        // from the cap arithmetic and signals a real defect. Classify it before
        // asserting so the next red CI run is self-diagnosing:
        //   consumed = rows removed (successful jobs are deleted)
        //   recorded = stats.succeeded
        //   recorded  > consumed        => a job was counted/delivered more than
        //                                  once (JoinSet outcome recorded twice,
        //                                  or a lease reclaim re-ran it).
        //   recorded == consumed > 100  => more than one drain contributed; a
        //                                  single drain cannot exceed the cap.
        let total = count(&pool, None).await;
        let consumed = seeded - total;

        if consumed != 100 {
            let ready = count(&pool, Some("ready")).await;
            let claimed = count(&pool, Some("claimed")).await;
            panic!(
                "over-cap trap: {}\n  stats (AGGREGATE across plugins): succeeded={} retried={} dead_lettered={} errors={}",
                classify_over_cap(
                    seeded,
                    120,
                    consumed,
                    stats.succeeded,
                    total,
                    ready,
                    claimed
                ),
                stats.succeeded,
                stats.retried,
                stats.dead_lettered,
                stats.errors,
            );
        }

        // The per-plugin cap, measured on THIS plugin's rows.
        assert_eq!(consumed, 100, "capped at 100 per cycle");

        // Reading the aggregate as this plugin's contribution is only valid
        // because this dispatcher loads exactly one `tap_queue_worker` plugin
        // (see `cap_is_measured_per_plugin_not_across_plugins`). Assert that
        // precondition rather than relying on it silently.
        assert_eq!(
            dispatcher().registry().handler_count("tap_queue_worker"),
            1,
            "this test reads aggregate stats as per-plugin; that needs a single worker"
        );
        assert_eq!(
            stats.succeeded, consumed as u64,
            "with one worker loaded, every consumed row is one recorded success \
             — a mismatch is a double-count or double-delivery"
        );

        assert_eq!(
            count(&pool, Some("ready")).await,
            20,
            "remainder waits for next cycle"
        );

        clean_queue(&pool).await;
    });
}

// ── Priority: higher-priority jobs are claimed first ─────────────────────────

#[test]
fn higher_priority_jobs_drain_first() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // 100 low-priority jobs (fills a full cycle) + 1 high-priority job inserted
        // last (newest created_at). Priority ordering must claim the high-priority
        // job within the first cycle despite its late arrival.
        for i in 0..100 {
            insert_job(
                &pool,
                serde_json::json!({"outcome": "ok", "i": i}),
                0,
                5,
                now(),
            )
            .await;
        }
        let hi = insert_job(
            &pool,
            serde_json::json!({"outcome": "ok", "hi": true}),
            10,
            5,
            now() + 1,
        )
        .await;

        // Same precondition capture as the flood test — see the note there.
        let seeded = count(&pool, None).await;

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        // Over-cap trap (P11d) — same invariant as per_plugin_cap_bounds_a_flood,
        // adapted to this test's 101 inserted jobs (100 low + 1 hi). A single
        // drain caps at MAX_QUEUE_ITEMS_PER_CYCLE (100); `succeeded != 100`
        // signals a real defect (this test observed 101 = the whole flood under
        // llvm-cov timing). Classify before asserting so the next red run names
        // the class instead of repeating as a mystery.
        let total = count(&pool, None).await;
        let consumed = seeded - total;

        if consumed != 100 {
            let ready = count(&pool, Some("ready")).await;
            let claimed = count(&pool, Some("claimed")).await;
            panic!(
                "over-cap trap: {}\n  stats (AGGREGATE across plugins): succeeded={} retried={} dead_lettered={} errors={}",
                classify_over_cap(
                    seeded,
                    101,
                    consumed,
                    stats.succeeded,
                    total,
                    ready,
                    claimed
                ),
                stats.succeeded,
                stats.retried,
                stats.dead_lettered,
                stats.errors,
            );
        }

        // Per-plugin cap on this plugin's own rows — see the flood test.
        assert_eq!(consumed, 100);
        assert_eq!(
            dispatcher().registry().handler_count("tap_queue_worker"),
            1,
            "this test reads aggregate stats as per-plugin; that needs a single worker"
        );
        assert_eq!(stats.succeeded, consumed as u64);

        // The high-priority job was consumed; a low-priority job remains instead.
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_queue WHERE id = $1")
            .bind(hi)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            remaining, 0,
            "high-priority job drained despite late arrival"
        );
        assert_eq!(
            count(&pool, Some("ready")).await,
            1,
            "one low-priority job left"
        );

        clean_queue(&pool).await;
    });
}
