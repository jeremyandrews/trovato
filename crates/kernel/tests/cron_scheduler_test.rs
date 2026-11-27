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
//! other caller. They share the `cron_interval_secs` row and the `cron:lock` /
//! `cron:last_run` keys, so they run one at a time.

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio_util::sync::CancellationToken;

use trovato_kernel::cron::{CRON_INTERVAL_CONFIG_KEY, CronResult, CronService};
use trovato_kernel::models::SiteConfig;

/// Mirrors the private `CRON_LOCK_KEY` in `crates/kernel/src/cron/mod.rs`.
const CRON_LOCK_KEY: &str = "cron:lock";

/// Mirrors the private key `CronService::record_run` writes.
const CRON_LAST_RUN_KEY: &str = "cron:last_run";

/// One test at a time: they share one `site_config` row and the cron keys.
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

async fn clear_cron_keys(conn: &mut redis::aio::MultiplexedConnection) {
    for key in [CRON_LOCK_KEY, CRON_LAST_RUN_KEY] {
        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(conn)
            .await
            .unwrap_or(());
    }
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
    assert!(
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .is_ok(),
        "the scheduler loop did not stop within 5 seconds of shutdown"
    );
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
/// HTTP call at all, and shutdown stops the loop. Red on `main`: nothing called
/// `run` on a timer, so `cron:last_run` stayed empty.
#[test]
fn the_scheduler_runs_cron_on_its_interval_with_no_http_call() {
    serial(async {
        let app = common::shared_app().await;
        let mut conn = redis_conn().await;
        clear_cron_keys(&mut conn).await;
        set_interval(app, 2).await;

        let cron = app.state.cron().clone();
        let (shutdown, handle) = spawn_scheduler(cron.clone());

        // Every distinct run the scheduler records. Nothing else here calls
        // `run`: no request is sent.
        let mut runs = BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        while tokio::time::Instant::now() < deadline && runs.len() < 2 {
            if let Ok(Some(run)) = cron.last_run().await {
                assert_eq!(run.status, "completed", "{run:?}");
                runs.insert(run.timestamp);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        stop(shutdown, handle).await;
        clear_cron_keys(&mut conn).await;
        assert!(
            runs.len() >= 2,
            "expected two scheduled runs within 12 seconds at a 2 second \
             interval; saw {runs:?}"
        );
        let first = *runs.iter().next().unwrap();
        let second = *runs.iter().nth(1).unwrap();
        assert!(
            second - first >= 2,
            "two scheduled runs came closer together than the interval: {runs:?}"
        );
    });
}

/// An external `POST /cron/{key}` that lands while a scheduled run is in
/// progress is skipped, not run alongside it.
///
/// The scheduled run is held in progress deterministically: the test holds an
/// exclusive lock on `file_managed`, which the run's first task (temporary
/// file cleanup) deletes from, so the run takes the cron lock and then waits.
#[test]
fn an_external_call_during_a_scheduled_run_is_skipped() {
    serial(async {
        let app = common::shared_app().await;
        let mut conn = redis_conn().await;
        clear_cron_keys(&mut conn).await;
        set_interval(app, 1).await;

        let mut blocker = app.db.begin().await.expect("begin");
        sqlx::query("LOCK TABLE file_managed IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .expect("lock file_managed");

        let cron = app.state.cron().clone();
        let (shutdown, handle) = spawn_scheduler(cron.clone());

        // Wait for the scheduled run to take the cron lock.
        let mut held = None;
        for _ in 0..100 {
            let value: Option<String> = redis::cmd("GET")
                .arg(CRON_LOCK_KEY)
                .query_async(&mut conn)
                .await
                .unwrap_or(None);
            if value.is_some() {
                held = value;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let Some(held) = held else {
            blocker.rollback().await.ok();
            stop(shutdown, handle).await;
            panic!("the scheduler never took the cron lock within 5 seconds");
        };

        let (status, body) = post_cron(app).await;

        // Let the scheduled run finish.
        blocker.rollback().await.expect("release file_managed");
        let mut finished = None;
        for _ in 0..100 {
            if let Ok(Some(run)) = cron.last_run().await {
                finished = Some(run);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        stop(shutdown, handle).await;
        clear_cron_keys(&mut conn).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["status"], "skipped",
            "an external call while the scheduler held the lock ({held}) ran \
             cron anyway: {body}"
        );
        let finished = finished.expect("the scheduled run never finished");
        assert_eq!(finished.status, "completed", "{finished:?}");
    });
}

/// Interval 0 is off: the loop never runs cron, and shutdown still stops it.
#[test]
fn interval_zero_means_no_scheduled_run() {
    serial(async {
        let app = common::shared_app().await;
        let mut conn = redis_conn().await;
        clear_cron_keys(&mut conn).await;
        set_interval(app, 0).await;

        let cron = app.state.cron().clone();
        let config = cron.scheduler_config().await;
        assert!(!config.enabled(), "{config:?}");

        let (shutdown, handle) = spawn_scheduler(cron.clone());
        tokio::time::sleep(Duration::from_secs(3)).await;
        let last_run = cron.last_run().await.expect("read last_run");
        stop(shutdown, handle).await;
        clear_cron_keys(&mut conn).await;

        assert!(
            last_run.is_none(),
            "a scheduler set to 0 (off) ran cron: {last_run:?}"
        );
    });
}

/// Shutdown stops a loop that is between runs at once, not at the end of its
/// interval.
#[test]
fn shutdown_stops_the_scheduler_between_runs() {
    serial(async {
        let app = common::shared_app().await;
        let mut conn = redis_conn().await;
        clear_cron_keys(&mut conn).await;
        set_interval(app, 3600).await;

        let cron = app.state.cron().clone();
        let (shutdown, handle) = spawn_scheduler(cron.clone());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!handle.is_finished(), "the scheduler loop ended on its own");

        shutdown.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(2), handle).await;
        let last_run = cron.last_run().await.expect("read last_run");
        clear_cron_keys(&mut conn).await;
        assert!(
            stopped.is_ok(),
            "the scheduler did not stop within 2 seconds of shutdown"
        );
        assert!(
            last_run.is_none(),
            "an hourly scheduler ran cron in its first second: {last_run:?}"
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
        let mut conn = redis_conn().await;
        clear_cron_keys(&mut conn).await;
        set_interval(app, 4).await;

        let cron = app.state.cron().clone();
        let (shutdown, handle) = spawn_scheduler(cron.clone());

        // An external run two seconds into the scheduler's first interval.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (status, body) = post_cron(app).await;
        assert_eq!(body["status"], "completed", "{status} {body}");
        let external = cron
            .last_run()
            .await
            .expect("read last_run")
            .expect("the external run was recorded")
            .timestamp;

        // The scheduler's own interval ends at about 4 seconds, when the
        // external run is about 2 seconds old: it must not run then.
        tokio::time::sleep(Duration::from_millis(3_000)).await;
        let at_five = cron
            .last_run()
            .await
            .expect("read last_run")
            .expect("a run is recorded")
            .timestamp;

        // Once the external run is an interval old, the scheduler runs.
        let mut later = at_five;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while tokio::time::Instant::now() < deadline && later == external {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Ok(Some(run)) = cron.last_run().await {
                later = run.timestamp;
            }
        }
        stop(shutdown, handle).await;
        clear_cron_keys(&mut conn).await;

        assert_eq!(
            at_five,
            external,
            "the scheduler ran again {} seconds after an external run, \
             inside its 4 second interval",
            at_five - external
        );
        assert!(
            later - external >= 4,
            "the scheduler did not run once the external run was an interval \
             old: external at {external}, next at {later}"
        );
    });
}

/// Two callers of `run` at once never both execute the cron tasks: one
/// completes, the other is skipped.
#[test]
fn two_concurrent_runs_never_both_execute() {
    serial(async {
        let app = common::shared_app().await;
        let mut conn = redis_conn().await;
        clear_cron_keys(&mut conn).await;

        let cron = app.state.cron().clone();
        let (a, b) = tokio::join!(cron.run(), cron.run());
        clear_cron_keys(&mut conn).await;

        let results = [a, b];
        let completed = results
            .iter()
            .filter(|r| matches!(r, CronResult::Completed { .. }))
            .count();
        let skipped = results
            .iter()
            .filter(|r| matches!(r, CronResult::Skipped))
            .count();
        assert_eq!((completed, skipped), (1, 1), "{results:?}");
    });
}
