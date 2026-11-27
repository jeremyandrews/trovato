#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The built-in cron scheduler (B178).
//!
//! A freshly installed site has to run its cron tasks on its own. Before this,
//! `CronService::run` was only ever called from `POST /cron/{key}` or the admin
//! "Run now" button, so a site with no external caller never published
//! scheduled items, never rebuilt its search index and never cleaned up after
//! itself. `CronService::run_scheduler` is the loop `main.rs` spawns at boot;
//! it calls the same `run` on the interval in the `cron_interval_secs`
//! `site_config` row.
//!
//! These drive the real loop against the real Postgres and Redis, and the
//! external caller is the real `/cron/{key}` route, because the property under
//! test is that the loop calls the same method, under the same lock, as every
//! other caller.
//!
//! Other suites run cron on the same site at the same time (they treat a
//! skipped run as another holder), so nothing here reads the site-wide
//! `cron:last_run` to count the scheduler's runs or clears the shared lock.
//! The scheduler's own runs are counted by
//! `CronService::scheduled_runs_completed`, which a foreign run can delay but
//! never inflate. The tests in this file share the `cron_interval_secs` row,
//! so they run one at a time: a mutex under libtest, the `cron-scheduler`
//! test group in `.config/nextest.toml` under nextest.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use trovato_kernel::cron::{CRON_INTERVAL_CONFIG_KEY, CronResult, CronService};
use trovato_kernel::models::SiteConfig;

/// Mirrors the private `CRON_LOCK_KEY` in `crates/kernel/src/cron/mod.rs`.
const CRON_LOCK_KEY: &str = "cron:lock";

/// One test at a time under libtest: they share one `site_config` row.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial<F: std::future::Future<Output = ()> + Send>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    common::run_test(body);
}

async fn redis_conn() -> redis::aio::MultiplexedConnection {
    redis::Client::open(trovato_test_utils::env::redis_url())
        .expect("redis client")
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection")
}

/// The prefix of every cron lock value this process writes: `host:pid:`.
fn own_lock_prefix() -> String {
    let host = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    format!("{host}:{}:", std::process::id())
}

/// Save the interval, and put the row back to absent (the default) when the
/// test ends, pass or fail.
async fn set_interval(app: &common::TestApp, interval_secs: u64) {
    SiteConfig::set(
        &app.db,
        CRON_INTERVAL_CONFIG_KEY,
        serde_json::json!(interval_secs),
    )
    .await
    .expect("save cron_interval_secs");
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM site_config WHERE key = $1")
            .bind(CRON_INTERVAL_CONFIG_KEY)
            .execute(&db)
            .await;
    });
}

/// A scheduler loop on its own task, as `main.rs` spawns it.
fn spawn_scheduler(cron: Arc<CronService>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn({
        let shutdown = shutdown.clone();
        async move { cron.run_scheduler(shutdown).await }
    });
    (shutdown, handle)
}

async fn stop(shutdown: CancellationToken, handle: tokio::task::JoinHandle<()>) {
    shutdown.cancel();
    let stopped = tokio::time::timeout(Duration::from_secs(10), handle).await;
    assert_clean_stop(stopped, "10 seconds");
}

/// The loop ended within the limit *and* returned normally: a panic in the
/// loop is a failure, not a stop.
fn assert_clean_stop(
    stopped: Result<Result<(), tokio::task::JoinError>, tokio::time::error::Elapsed>,
    limit: &str,
) {
    match stopped {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("the scheduler loop panicked instead of stopping: {e}"),
        Err(_) => panic!("the scheduler loop did not stop within {limit} of shutdown"),
    }
}

async fn lock_value(conn: &mut redis::aio::MultiplexedConnection) -> Option<String> {
    redis::cmd("GET")
        .arg(CRON_LOCK_KEY)
        .query_async(conn)
        .await
        .unwrap_or(None)
}

/// Wait until the scheduler has completed more than `count` runs, up to
/// `limit`; the instant it was seen, or `None`.
async fn wait_for_runs_past(cron: &CronService, count: u64, limit: Duration) -> Option<Instant> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if cron.scheduled_runs_completed() > count {
            return Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

async fn post_cron(app: &common::TestApp) -> (StatusCode, serde_json::Value) {
    let key = app.state.runtime().cron_key.clone();
    let response = app
        .request(
            Request::builder()
                .method("POST")
                .uri(format!("/cron/{key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    (
        status,
        serde_json::from_slice(&bytes).expect("cron response is JSON"),
    )
}

/// With the scheduler on and a short interval, cron runs repeatedly with no
/// HTTP call at all, at least an interval apart, and shutdown stops the loop.
/// Red on `main`: nothing called `run` on a timer.
#[test]
fn the_scheduler_runs_cron_on_its_interval_with_no_http_call() {
    serial(async {
        let app = common::shared_app().await;
        set_interval(app, 2).await;

        let cron = app.state.cron().clone();
        let before = cron.scheduled_runs_completed();
        let (shutdown, handle) = spawn_scheduler(cron.clone());

        // Nothing in this test sends a request. Another suite's cron can only
        // delay a scheduled run, hence the generous limit.
        let first = wait_for_runs_past(&cron, before, Duration::from_secs(60)).await;
        let second = wait_for_runs_past(&cron, before + 1, Duration::from_secs(60)).await;
        stop(shutdown, handle).await;

        let first = first.expect("the scheduler never ran cron");
        let second = second.expect("the scheduler ran cron once, never twice");
        assert!(
            second - first >= Duration::from_millis(1_900),
            "two scheduled runs came {:?} apart, inside the 2 second interval",
            second - first
        );
    });
}

/// An external `POST /cron/{key}` that lands while a scheduled run is in
/// progress is skipped, not run alongside it.
///
/// The scheduled run is held in progress deterministically: the test holds an
/// exclusive lock on `file_managed`, which the run's first task (temporary
/// file cleanup) reads, so the run takes the cron lock and then waits.
#[test]
fn an_external_call_during_a_scheduled_run_is_skipped() {
    serial(async {
        let app = common::shared_app().await;
        let mut conn = redis_conn().await;
        set_interval(app, 1).await;

        let cron = app.state.cron().clone();
        let before = cron.scheduled_runs_completed();
        let (shutdown, handle) = spawn_scheduler(cron.clone());

        // Hold an exclusive lock on `file_managed` so the scheduler's run
        // stops at it, and wait for that run to hold the cron lock. Nothing
        // else in this process runs cron, so a lock value with this process's
        // prefix is the scheduler's. Another suite's run would block on the
        // table lock too, holding the cron lock the scheduler is waiting for,
        // so the table lock is only taken while nobody holds the cron lock and
        // is let go the moment a foreign run is seen holding it.
        let prefix = own_lock_prefix();
        let mut blocker: Option<sqlx::Transaction<'_, sqlx::Postgres>> = None;
        let mut held = None;
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            let value = lock_value(&mut conn).await;
            match value {
                Some(v) if v.starts_with(&prefix) => {
                    if blocker.is_some() {
                        held = Some(v);
                        break;
                    }
                    // Our run started before the table lock: let it finish.
                }
                Some(_) => {
                    if let Some(tx) = blocker.take() {
                        tx.rollback().await.ok();
                    }
                }
                None if blocker.is_none() => {
                    let mut tx = app.db.begin().await.expect("begin");
                    sqlx::query("LOCK TABLE file_managed IN ACCESS EXCLUSIVE MODE")
                        .execute(&mut *tx)
                        .await
                        .expect("lock file_managed");
                    // A run that took the cron lock before the table lock may
                    // already be past `file_managed`: start over.
                    if lock_value(&mut conn).await.is_some() {
                        tx.rollback().await.ok();
                    } else {
                        blocker = Some(tx);
                    }
                }
                None => {}
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (Some(held), Some(blocker)) = (held, blocker) else {
            stop(shutdown, handle).await;
            panic!(
                "the scheduler never took the cron lock behind the table lock within 90 seconds"
            );
        };

        let (status, body) = post_cron(app).await;

        // Let the scheduled run finish.
        blocker.rollback().await.expect("release file_managed");
        let finished = wait_for_runs_past(&cron, before, Duration::from_secs(30)).await;
        stop(shutdown, handle).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["status"], "skipped",
            "an external call while the scheduler held the lock ({held}) ran \
             cron anyway: {body}"
        );
        assert!(
            finished.is_some(),
            "the scheduled run never completed once it was let go"
        );
    });
}

/// Interval 0 is off: the loop never runs cron, and shutdown still stops it.
#[test]
fn interval_zero_means_no_scheduled_run() {
    serial(async {
        let app = common::shared_app().await;
        set_interval(app, 0).await;

        let cron = app.state.cron().clone();
        let config = cron.scheduler_config().await;
        assert!(!config.enabled(), "{config:?}");

        let before = cron.scheduled_runs_completed();
        let (shutdown, handle) = spawn_scheduler(cron.clone());
        tokio::time::sleep(Duration::from_secs(3)).await;
        let after = cron.scheduled_runs_completed();
        stop(shutdown, handle).await;

        assert_eq!(
            after,
            before,
            "a scheduler set to 0 (off) ran cron {} times",
            after - before
        );
    });
}

/// Shutdown stops a loop that is between runs at once, not at the end of its
/// interval.
#[test]
fn shutdown_stops_the_scheduler_between_runs() {
    serial(async {
        let app = common::shared_app().await;
        set_interval(app, 3600).await;

        let cron = app.state.cron().clone();
        let before = cron.scheduled_runs_completed();
        let (shutdown, handle) = spawn_scheduler(cron.clone());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!handle.is_finished(), "the scheduler loop ended on its own");

        shutdown.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert_clean_stop(stopped, "2 seconds");
        assert_eq!(
            cron.scheduled_runs_completed(),
            before,
            "an hourly scheduler ran cron in its first second"
        );
    });
}

/// A scheduled tick that comes right after an external run waits that run out
/// instead of running again, so an external caller, or a second server's
/// scheduler, does not add to the cadence.
#[test]
fn a_scheduled_run_waits_out_a_recent_external_run() {
    serial(async {
        let app = common::shared_app().await;
        set_interval(app, 4).await;

        let cron = app.state.cron().clone();
        let before = cron.scheduled_runs_completed();
        let (shutdown, handle) = spawn_scheduler(cron.clone());

        // An external run about two seconds into the scheduler's first
        // interval. Another suite holding the lock makes it skip; try again.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut external_done = None;
        for _ in 0..120 {
            let (status, body) = post_cron(app).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            if body["status"] == "completed" {
                external_done = Some(Instant::now());
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let external_done = external_done.expect("the external run never got the lock");

        // The scheduler's own interval ends while the external run is younger
        // than 4 seconds: it must hold off until that run is an interval old.
        let next = wait_for_runs_past(&cron, before, Duration::from_secs(60)).await;
        stop(shutdown, handle).await;

        let next = next.expect("the scheduler never ran after the external run");
        // `cron:last_run` is in whole seconds, so allow one second of rounding.
        assert!(
            next - external_done >= Duration::from_secs(3),
            "the scheduler ran {:?} after an external run, inside its 4 second \
             interval",
            next - external_done
        );
    });
}

/// Two callers of `run` at once never both execute the cron tasks.
#[test]
fn two_concurrent_runs_never_both_execute() {
    serial(async {
        let app = common::shared_app().await;
        let cron = app.state.cron().clone();

        // Another suite holding the lock skips both; try until one of ours
        // gets it, and every time at most one may complete.
        let mut seen_one = false;
        for _ in 0..120 {
            let (a, b) = tokio::join!(cron.run(), cron.run());
            let results = [a, b];
            let completed = results
                .iter()
                .filter(|r| matches!(r, CronResult::Completed { .. }))
                .count();
            assert!(completed <= 1, "both concurrent runs executed: {results:?}");
            if completed == 1 {
                seen_one = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(seen_one, "neither run ever got the lock");
    });
}

/// Shutdown that arrives while the loop is waiting on its settings read stops
/// the loop there, and no run starts after it.
///
/// The read is held up with an exclusive lock on `site_config`, kept only for
/// about a second since every other suite reads that table too.
#[test]
fn shutdown_during_a_settings_read_starts_no_run() {
    serial(async {
        let app = common::shared_app().await;
        set_interval(app, 1).await;

        let cron = app.state.cron().clone();
        let before = cron.scheduled_runs_completed();
        let (shutdown, handle) = spawn_scheduler(cron.clone());
        wait_for_runs_past(&cron, before, Duration::from_secs(60))
            .await
            .expect("the scheduler never ran cron");
        let ran = cron.scheduled_runs_completed();

        // The loop wakes within a second of that run and reads its setting.
        let mut blocker = app.db.begin().await.expect("begin");
        sqlx::query("LOCK TABLE site_config IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .expect("lock site_config");
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        shutdown.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(2), handle).await;
        blocker.rollback().await.expect("release site_config");

        // Long enough for a run the loop still started to finish.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_clean_stop(stopped, "2 seconds, with its settings read blocked,");
        assert_eq!(
            cron.scheduled_runs_completed(),
            ran,
            "the scheduler ran cron after shutdown"
        );
    });
}
