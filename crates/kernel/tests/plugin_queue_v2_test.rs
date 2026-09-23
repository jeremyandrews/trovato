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

/// Epoch budget (seconds) this suite gives a background tap.
///
/// The shipped default is 150, which is the *point* of the `"spin"` fixture arm
/// and also far too long to wait for in a test. Lowering it here exercises the
/// identical code path at a size a test can observe. It stays well clear of any
/// legitimate dispatch in this file — every other payload returns in
/// milliseconds — so nothing else can be mistaken for CPU exhaustion even on a
/// loaded CI runner.
const TEST_TAP_BUDGET_SECS: u64 = 5;

/// Build (once) a dispatcher with only the queue-v2 fixture loaded.
fn dispatcher() -> Arc<TapDispatcher> {
    FIXTURE_DISPATCHER
        .get_or_init(|| {
            let config = PluginConfig {
                limits: trovato_kernel::plugin::limits::ResourceLimits {
                    background_tap_epoch_deadline_secs: TEST_TAP_BUDGET_SECS,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut runtime = PluginRuntime::new(&config).expect("create runtime");
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

// ── max_attempts bounds the CLAIM, not only the failure bookkeeping ──────────

/// Insert a row with explicit lifecycle columns; returns its id. The v2 tests
/// above all start from a pristine `ready` row, which cannot express the state
/// this section is about: a job that was claimed and never heard from again.
async fn insert_raw_job(
    pool: &PgPool,
    payload: serde_json::Value,
    attempts: i32,
    max_attempts: i32,
    status: &str,
    locked_until: i64,
) -> i64 {
    let row = sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, max_attempts,
             attempts, next_attempt_at, status, locked_until)
        VALUES ($1, 'test_queue', $2, $3, 0, $4, $5, 0, $6, $7)
        RETURNING id
        "#,
    )
    .bind(FIXTURE)
    .bind(&payload)
    .bind(now())
    .bind(max_attempts)
    .bind(attempts)
    .bind(status)
    .bind(locked_until)
    .fetch_one(pool)
    .await
    .unwrap();
    row.get::<i64, _>("id")
}

/// A job that reached `max_attempts` while claimed, and whose lease then
/// expired, must be RETIRED — never handed to a worker again.
///
/// This is the defect that turned a transient worker stall into a permanent
/// outage. `claim_batch` selected on `status`/`next_attempt_at`/`locked_until`
/// alone, with no `attempts < max_attempts` term, and incremented `attempts`
/// unconditionally. `max_attempts` was consulted *only* by `mark_job_failed`,
/// on the failure path — which never runs for a claimer that does not return.
/// So a row whose claimer vanished was re-dispatched on every cycle forever,
/// `attempts` climbing past its own bound and `dead_at` staying null, while its
/// worker slot was never released. Four such rows held all four slots of the
/// kernel concurrency cap and starved every other queue behind them.
///
/// The observable contract this pins: the row is `dead`, it carries a
/// `dead_reason` and a `dead_at`, its `attempts` did NOT advance (it was never
/// dispatched again), and the drain reports no work done on it.
#[test]
fn exhausted_job_is_retired_not_reclaimed() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // attempts == max_attempts, claimed, lease long expired: exactly the
        // rows found wedged in production.
        let id = insert_raw_job(
            &pool,
            serde_json::json!({"outcome": "ok"}),
            5,
            5,
            "claimed",
            now() - 1000,
        )
        .await;

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        // Name the pre-fix failure rather than panicking on a bare RowNotFound:
        // under the defect the row is re-claimed, dispatched, succeeds and is
        // DELETED, so `row_state` finds nothing. That deletion IS the bug, and a
        // red run should say which defect it is looking at.
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_queue WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            present, 1,
            "the job was dispatched a 6th time against a 5-attempt bound and consumed: \
             max_attempts is bounding only the failure path, not the claim"
        );

        let (status, attempts, _next, _last_error, dead_reason) = row_state(&pool, id).await;

        assert_eq!(
            status, "dead",
            "an exhausted, abandoned job must be retired; it was re-dispatched instead"
        );
        assert_eq!(
            attempts, 5,
            "attempts must not advance past max_attempts: a retired job is never dispatched again"
        );
        assert!(
            dead_reason.is_some(),
            "a retired job must record why it died, or it is invisible to the DLQ screen"
        );
        let dead_at: Option<i64> =
            sqlx::query_scalar("SELECT dead_at FROM plugin_queue WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(dead_at.is_some(), "a retired job must carry dead_at");

        assert_eq!(
            stats.succeeded, 0,
            "the worker must not have run: this job had already spent its attempts"
        );

        clean_queue(&pool).await;
    });
}

/// The bound must not break crash recovery. A job UNDER `max_attempts` whose
/// lease expired is still reclaimable — that is the at-least-once guarantee
/// (D-47), and the fence above must not be mistaken for "never reclaim a
/// claimed row".
#[test]
fn unexhausted_job_with_expired_lease_is_still_reclaimed() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // One attempt spent of five, claimer crashed, lease expired.
        let id = insert_raw_job(
            &pool,
            serde_json::json!({"outcome": "ok"}),
            1,
            5,
            "claimed",
            now() - 1000,
        )
        .await;

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM plugin_queue WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            remaining, 0,
            "a job with attempts left must still be reclaimed and run after a crashed claimer"
        );
        assert_eq!(
            stats.succeeded, 1,
            "the reclaimed job should have succeeded"
        );

        clean_queue(&pool).await;
    });
}

/// A retired job stays retired. The reaper runs at the head of every drain, so
/// it must be idempotent: a second pass must not re-stamp `dead_at`, resurrect
/// the row, or report it as fresh work.
#[test]
fn a_retired_job_stays_retired_across_drains() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        let id = insert_raw_job(
            &pool,
            serde_json::json!({"outcome": "ok"}),
            5,
            5,
            "claimed",
            now() - 1000,
        )
        .await;

        let cron = cron_with(pool.clone(), dispatcher());
        cron.drain_plugin_queues().await.unwrap();
        let first: Option<i64> =
            sqlx::query_scalar("SELECT dead_at FROM plugin_queue WHERE id = $1")
                .bind(id)
                .fetch_optional(&pool)
                .await
                .unwrap()
                .unwrap_or_else(|| {
                    panic!(
                        "the exhausted job was re-dispatched and consumed by the first drain, \
                     so there is nothing left to stay retired"
                    )
                });

        let stats = cron.drain_plugin_queues().await.unwrap();
        let (status, attempts, _n, _l, _d) = row_state(&pool, id).await;
        let second: Option<i64> =
            sqlx::query_scalar("SELECT dead_at FROM plugin_queue WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(status, "dead", "the row must stay dead");
        assert_eq!(attempts, 5, "a dead row is never dispatched again");
        assert_eq!(
            first, second,
            "dead_at must not be re-stamped by a later drain"
        );
        assert_eq!(stats.total(), 0, "a dead row is not work");

        clean_queue(&pool).await;
    });
}

// ── Concurrency is declared PER QUEUE, and honored per queue ─────────────────

/// The fixture's second queue, declared at concurrency 1 where `test_queue` is
/// declared at 8. Two queues on one plugin with different declarations is the
/// shape the collapse hid.
const SERIAL_QUEUE: &str = "test_serial_queue";

/// Insert one job onto a named queue; returns its id.
async fn insert_on_queue(pool: &PgPool, queue_name: &str, payload: serde_json::Value) -> i64 {
    let row = sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, max_attempts,
             attempts, next_attempt_at, status, locked_until)
        VALUES ($1, $2, $3, $4, 0, 5, 0, 0, 'ready', 0)
        RETURNING id
        "#,
    )
    .bind(FIXTURE)
    .bind(queue_name)
    .bind(&payload)
    .bind(now())
    .fetch_one(pool)
    .await
    .unwrap();
    row.get::<i64, _>("id")
}

/// Each declared queue keeps its OWN width.
///
/// `tap_queue_info` has always returned one entry per queue, each with its own
/// `concurrency`. The kernel read the **maximum** across those entries, once per
/// plugin, and applied it to a claim that spanned every one of the plugin's
/// queues. So a plugin declaring `analyze: 4, cluster: 1, summarize: 1` ran
/// `cluster` four wide, because the width it got was `analyze`'s. Per-queue
/// declarations bounded nothing (`G-QUEUE-CONCURRENCY-COLLAPSED`).
///
/// The fixture declares `test_queue` at 8 and `test_serial_queue` at 1. Under
/// the collapse both resolve to 4 (the max, clamped). They must not.
#[test]
fn declared_widths_are_per_queue_not_per_plugin() {
    serial(async {
        let pool = fresh_pool().await;
        let cron = cron_with(pool.clone(), dispatcher());

        let widths = cron.resolved_queue_widths(FIXTURE).await;

        assert_eq!(
            widths.get("test_queue").copied(),
            Some(4),
            "a declaration over the kernel cap is clamped to it"
        );
        assert_eq!(
            widths.get(SERIAL_QUEUE).copied(),
            Some(1),
            "a queue declared at 1 must be honored at 1; getting the plugin's \
             maximum here is the per-queue collapse"
        );
        assert_eq!(
            widths.len(),
            2,
            "both declared queues should be resolved, and nothing else"
        );
    });
}

/// A queue with rows but no declaration still drains, at width 1.
///
/// The claim is now scoped to a queue name, so a row on a queue the plugin never
/// declared could have been stranded by a declaration it has no say in. It is
/// not: an undeclared queue drains at the conservative width.
#[test]
fn an_undeclared_queue_still_drains() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        for _ in 0..3 {
            insert_on_queue(
                &pool,
                "never_declared",
                serde_json::json!({"outcome": "ok"}),
            )
            .await;
        }

        let cron = cron_with(pool.clone(), dispatcher());
        let stats = cron.drain_plugin_queues().await.unwrap();

        assert_eq!(
            stats.succeeded, 3,
            "an undeclared queue must not be stranded"
        );
        assert_eq!(count(&pool, None).await, 0);

        // And it is not in the declared map, which is what makes it width 1.
        assert!(
            !cron
                .resolved_queue_widths(FIXTURE)
                .await
                .contains_key("never_declared")
        );

        clean_queue(&pool).await;
    });
}

/// Every queue of a plugin makes progress in one cycle.
///
/// Giving each queue its own width means the shared per-plugin budget is spent
/// across several queues, and spending it one queue at a time in name order
/// would let the first queue starve the rest. The queues take turns instead, so
/// a cycle that cannot finish everything still advances all of them.
#[test]
fn every_queue_of_a_plugin_advances_in_one_cycle() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        // More than the per-plugin cycle cap on the first queue alphabetically,
        // plus a little work on the last. Spent in name order, the 100-row queue
        // would consume the whole budget and the other would not move.
        for i in 0..100 {
            insert_on_queue(
                &pool,
                "aaa_first",
                serde_json::json!({"outcome": "ok", "i": i}),
            )
            .await;
        }
        for i in 0..3 {
            insert_on_queue(
                &pool,
                "zzz_last",
                serde_json::json!({"outcome": "ok", "i": i}),
            )
            .await;
        }

        let cron = cron_with(pool.clone(), dispatcher());
        cron.drain_plugin_queues().await.unwrap();

        let last_left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM plugin_queue WHERE plugin_name = $1 AND queue_name = 'zzz_last'",
        )
        .bind(FIXTURE)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(
            last_left, 0,
            "the alphabetically last queue starved behind the first: the per-plugin \
             budget is being spent one queue at a time instead of in turns"
        );

        clean_queue(&pool).await;
    });
}

// ── A job that burns its CPU budget, and the drain that used to wait for it ──

/// A worker that spends its entire CPU budget is dead-lettered on the spot.
///
/// This is the wedge, reduced to one job. `tap_queue_worker` is bounded only by
/// the background epoch deadline, so a worker that burns CPU occupies its slot
/// for the whole budget and is then cut off. The kernel recorded that as an
/// ordinary failed attempt and rescheduled it, which bought nothing: the retry
/// gets the same full budget and burns it against the same wall, holding a
/// worker slot each time, until `max_attempts` finally ran out — five times the
/// budget later, if it ever got there at all.
///
/// Measured on the shipped 150-second budget before this fix: the drain returned
/// after 150.2 seconds with one core pinned at 100%, and the job came back
/// `ready` with `attempts = 1` for another go.
///
/// A job cut off at its budget now dies immediately, whatever `attempts` says.
/// The bound it breached is not the attempt count.
#[test]
fn a_job_that_burns_its_cpu_budget_is_dead_lettered_not_retried() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        let id = insert_job(&pool, serde_json::json!({"outcome": "spin"}), 0, 5, now()).await;

        let cron = cron_with(pool.clone(), dispatcher());
        let started = std::time::Instant::now();
        let stats = cron.drain_plugin_queues().await.unwrap();
        let elapsed = started.elapsed();

        let (status, attempts, _next, last_error, dead_reason) = row_state(&pool, id).await;

        assert_eq!(
            status, "dead",
            "a job cut off at its CPU budget was rescheduled instead of dead-lettered: \
             the retry will burn the same budget again and hold a worker slot again"
        );
        assert_eq!(
            stats.dead_lettered, 1,
            "the drain should report one dead-lettered job"
        );
        assert_eq!(stats.retried, 0, "it must not be counted as a retry");
        assert_eq!(
            attempts, 1,
            "it died on its first attempt, not after exhausting max_attempts"
        );
        let reason = dead_reason.expect("a dead job must say why it died");
        assert!(
            reason.contains("CPU budget"),
            "the reason must name the CPU budget, not a generic failure: {reason}"
        );
        assert_eq!(last_error.as_deref(), Some(reason.as_str()));

        // It really did run to the budget; this is not some faster failure path.
        assert!(
            elapsed.as_secs_f64() >= TEST_TAP_BUDGET_SECS as f64 * 0.8,
            "expected the job to run to its {TEST_TAP_BUDGET_SECS}s budget, took {elapsed:?}"
        );

        clean_queue(&pool).await;
    });
}

/// One drain pass gives the cron lock back on a bound of its own.
///
/// `MAX_QUEUE_ITEMS_PER_CYCLE` bounded a pass in *items*, never in time, and a
/// single item may legitimately occupy a worker for the whole epoch budget. So a
/// pass could run for `items / width * budget` — over an hour at the shipped
/// values — and it ran inside a cron run holding the global cron lock. For that
/// whole time every other trigger answered "another instance is running cron",
/// `tap_cron` never dispatched, and a plugin's own stuck-queue alarm (which
/// lives in `tap_cron`) could neither observe the condition nor report it. That
/// is why the alert fired once and never again.
///
/// Eight CPU-burning jobs on a width-1 queue is 8 × the budget of work. The pass
/// must stop at its own budget and leave the rest for the next cycle.
#[test]
fn a_drain_pass_stops_at_its_time_budget() {
    serial(async {
        let pool = fresh_pool().await;
        clean_queue(&pool).await;

        for _ in 0..8 {
            insert_on_queue(&pool, SERIAL_QUEUE, serde_json::json!({"outcome": "spin"})).await;
        }

        let mut cron = CronService::new(
            redis::Client::open("redis://127.0.0.1:6379").expect("redis client"),
            pool.clone(),
        );
        cron.set_tap_dispatcher(dispatcher());
        // A budget below one dispatch, so the pass stops after the batch in
        // flight rather than starting another.
        cron.set_drain_budget(std::time::Duration::from_millis(1));
        let cron = Arc::new(cron);

        let started = std::time::Instant::now();
        cron.drain_plugin_queues().await.unwrap();
        let elapsed = started.elapsed();

        // The budget bounds how many more batches are STARTED, not the one in
        // flight, so one dispatch is expected; eight are not.
        let ceiling = TEST_TAP_BUDGET_SECS as f64 * 2.5;
        assert!(
            elapsed.as_secs_f64() < ceiling,
            "the pass ran {elapsed:?}, past its budget plus one dispatch: it is still \
             unbounded in time and is holding the cron lock for all of it"
        );

        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM plugin_queue WHERE plugin_name = $1 AND status <> 'dead'",
        )
        .bind(FIXTURE)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            left > 0,
            "the pass consumed the whole queue instead of stopping at its budget"
        );

        clean_queue(&pool).await;
    });
}
