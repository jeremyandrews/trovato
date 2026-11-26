#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The built-in cron scheduler (B178).
//!
//! A freshly installed site has to run its cron tasks on its own: before this,
//! `CronService::run` was only ever called from `POST /cron/{key}` or the admin
//! "Run now" button, so a site with no external caller configured never
//! published scheduled items, never rebuilt its search index and never cleaned
//! up after itself. `CronService::run_scheduler` is a boot-started loop that
//! calls the identical `run` on its own cadence, read from the `cron_scheduler`
//! `site_config` key (B178).
//!
//! These tests drive the real loop against a real Postgres and Redis — not a
//! stub of `run` — because the property under test is specifically that the
//! loop calls the same method, and is bound by the same lock, as every other
//! caller. All three tests share the `cron_scheduler` site_config row and the
//! `cron:lock` / `cron:last_run` Redis keys, so they are serialized at the
//! OS-thread level like the other suites that share cron's global state
//! (`plugin_queue_v2_test.rs`, `plugin_mail_rate_limit_test.rs`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use trovato_kernel::cron::{CronResult, CronService};
use trovato_kernel::models::SiteConfig;

/// Mirrors the private `CRON_LOCK_KEY` in `crates/kernel/src/cron/mod.rs`.
const CRON_LOCK_KEY: &str = "cron:lock";

/// Mirrors the private `cron:last_run` key `CronService::record_run` writes.
const CRON_LAST_RUN_KEY: &str = "cron:last_run";

/// Mirrors the private `CRON_SCHEDULER_CONFIG_KEY` in `crates/kernel/src/cron/mod.rs`.
const CRON_SCHEDULER_CONFIG_KEY: &str = "cron_scheduler";

/// Serializes these tests: they share one `site_config` row and one Redis
/// lock/last-run pair.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime");
    rt.block_on(body);
}

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

fn redis_client() -> redis::Client {
    redis::Client::open(trovato_test_utils::env::redis_url()).expect("redis client")
}

async fn clear_redis(conn: &mut redis::aio::MultiplexedConnection) {
    let _: () = redis::cmd("DEL")
        .arg(CRON_LOCK_KEY)
        .query_async(conn)
        .await
        .unwrap_or(());
    let _: () = redis::cmd("DEL")
        .arg(CRON_LAST_RUN_KEY)
        .query_async(conn)
        .await
        .unwrap_or(());
}

async fn set_scheduler_interval(pool: &PgPool, interval_secs: u64) {
    SiteConfig::set(
        pool,
        CRON_SCHEDULER_CONFIG_KEY,
        serde_json::json!({ "interval_secs": interval_secs }),
    )
    .await
    .expect("save cron_scheduler site_config");
}

/// Two callers of `run` at once — one of them the scheduler's situation
/// exactly, since the scheduler calls nothing but `run` — must never both
/// execute the cron tasks. Red before the lock existed (it is pre-existing
/// Redis `SET NX` behaviour): this proves the scheduler inherits it rather
/// than, say, a scheduler-local flag that only guards against itself.
#[test]
fn concurrent_runs_never_both_execute() {
    serial(async {
        let pool = fresh_pool().await;
        let redis = redis_client();
        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .expect("redis connection");
        clear_redis(&mut conn).await;

        let cron_a = Arc::new(CronService::new(redis.clone(), pool.clone()));
        let cron_b = Arc::new(CronService::new(redis.clone(), pool.clone()));

        let (result_a, result_b) = tokio::join!(cron_a.run(), cron_b.run());

        let results = [result_a, result_b];
        let completed = results
            .iter()
            .filter(|r| matches!(r, CronResult::Completed { .. }))
            .count();
        let skipped = results
            .iter()
            .filter(|r| matches!(r, CronResult::Skipped))
            .count();

        clear_redis(&mut conn).await;

        assert_eq!(
            completed, 1,
            "exactly one of two concurrent callers should have run the cron \
             tasks; got {results:?}"
        );
        assert_eq!(
            skipped, 1,
            "the other concurrent caller should have been told another \
             instance is running cron; got {results:?}"
        );
    });
}

/// With the scheduler enabled and a short interval, `run` fires repeatedly on
/// its own — no HTTP call anywhere in this test, no axum app even built — and
/// shutdown stops the loop promptly.
#[test]
fn scheduler_runs_on_its_own_interval_with_no_http_call() {
    serial(async {
        let pool = fresh_pool().await;
        let redis = redis_client();
        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .expect("redis connection");
        clear_redis(&mut conn).await;
        set_scheduler_interval(&pool, 2).await;

        let cron = Arc::new(CronService::new(redis.clone(), pool.clone()));
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn({
            let cron = cron.clone();
            let shutdown = shutdown.clone();
            async move { cron.run_scheduler(shutdown).await }
        });

        // Poll `last_run` well past two intervals, collecting every distinct
        // timestamp the scheduler records. No HTTP route is ever touched: the
        // only thing driving `run` here is the scheduler loop itself.
        let mut timestamps = std::collections::BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(7);
        while tokio::time::Instant::now() < deadline && timestamps.len() < 2 {
            if let Ok(Some(run)) = cron.last_run().await {
                timestamps.insert(run.timestamp);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        shutdown.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(2), handle).await;
        clear_redis(&mut conn).await;

        assert!(
            stopped.is_ok(),
            "the scheduler loop did not stop within 2 seconds of cancellation"
        );
        assert!(
            timestamps.len() >= 2,
            "expected at least two distinct scheduled runs in 7 seconds at a \
             2-second interval; saw {}: {timestamps:?}",
            timestamps.len()
        );
    });
}

/// `interval_secs == 0` (off) must never trigger a scheduled run, however long
/// the loop is left running, and shutdown still stops it cleanly.
#[test]
fn scheduler_interval_zero_means_no_scheduled_run() {
    serial(async {
        let pool = fresh_pool().await;
        let redis = redis_client();
        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .expect("redis connection");
        clear_redis(&mut conn).await;
        set_scheduler_interval(&pool, 0).await;

        let cron = Arc::new(CronService::new(redis.clone(), pool.clone()));
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn({
            let cron = cron.clone();
            let shutdown = shutdown.clone();
            async move { cron.run_scheduler(shutdown).await }
        });

        // Long enough to prove nothing happens; far short of the fallback
        // poll interval the loop uses while disabled, so a run here could only
        // be the scheduler misreading "off" as "on".
        tokio::time::sleep(Duration::from_secs(2)).await;

        let last_run = cron.last_run().await.expect("read last_run");

        shutdown.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(2), handle).await;
        clear_redis(&mut conn).await;

        assert!(
            stopped.is_ok(),
            "the scheduler loop did not stop within 2 seconds of cancellation"
        );
        assert!(
            last_run.is_none(),
            "a disabled scheduler (interval_secs = 0) ran cron anyway: {last_run:?}"
        );
    });
}
