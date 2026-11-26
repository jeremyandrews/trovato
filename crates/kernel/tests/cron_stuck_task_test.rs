#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A cron task stuck on Redis is cut off, and a held lock says how old it is
//! (B247).
//!
//! In the incident behind this, one run sat in `process_queues` for ten hours
//! waiting on a Redis that had stopped answering. Its heartbeat kept the lock
//! alive, and every later tick was skipped with nothing above debug in the
//! log. These tests give a run a queue that never answers while its lock still
//! works, and check that the task is timed out, the run finishes and releases
//! the lock straight away, and the next run goes ahead. Then they plant a lock
//! that has been held far too long and check that the skip is logged at warn.
//!
//! Requires Postgres + Redis; runs in CI.

mod common;

#[path = "common/fake_redis.rs"]
mod fake_redis;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{run_test, shared_app};
use fake_redis::{Mode, cpu_time, fake_redis};
use trovato_kernel::cron::{CronResult, CronService};

/// Mirrors the private `CRON_LOCK_KEY` in `crates/kernel/src/cron/mod.rs`.
const CRON_LOCK_KEY: &str = "cron:lock";

/// Every test here takes and releases the one global cron lock.
static CRON_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn redis_conn() -> redis::aio::MultiplexedConnection {
    use trovato_kernel::redis_conn::BoundedRedis;
    redis::Client::open(trovato_test_utils::env::redis_url())
        .unwrap()
        .bounded_connection()
        .await
        .expect("redis connection")
}

async fn clear_lock() {
    let _: () = redis::cmd("DEL")
        .arg(CRON_LOCK_KEY)
        .query_async(&mut redis_conn().await)
        .await
        .unwrap_or(());
}

async fn lock_holder() -> Option<String> {
    redis::cmd("GET")
        .arg(CRON_LOCK_KEY)
        .query_async(&mut redis_conn().await)
        .await
        .unwrap()
}

fn defer_clear_lock() {
    common::defer_cleanup(|| async { clear_lock().await });
}

/// A run whose queue never answers times `process_queues` out, finishes,
/// releases the lock at once, and leaves the next run free to go.
///
/// Before tasks had a bound, `process_queues` waited on the silent queue for
/// ever and so did the run; with its heartbeat renewing the lock, no other run
/// could start.
#[test]
fn a_task_stuck_on_redis_is_timed_out_and_the_next_run_proceeds() {
    run_test(async {
        // Held until the cleanups below have run: released at the end of the body,
        // this test's `DEL cron:lock` would land in the middle of the next test.
        common::hold_until_cleanup(CRON_LOCK.lock().await);
        let app = shared_app().await;
        clear_lock().await;
        defer_clear_lock();

        let stuck_queue = fake_redis(Mode::SilentOnCommand).await;
        let mut cron = CronService::new(
            redis::Client::open(trovato_test_utils::env::redis_url()).unwrap(),
            app.db.clone(),
        );
        cron.set_queue_redis(redis::Client::open(stuck_queue.as_str()).unwrap());
        cron.set_task_timeout(Duration::from_secs(1));
        let cron = Arc::new(cron);

        let started = Instant::now();
        let cpu_before = cpu_time();
        let first = tokio::time::timeout(Duration::from_secs(120), cron.run())
            .await
            .expect("the run never came back: a stuck task holds the whole run");
        let cpu = cpu_time().saturating_sub(cpu_before);
        eprintln!(
            "first run: {first:?} in {:?}, cpu {cpu:?}",
            started.elapsed()
        );

        let CronResult::Completed { tasks_run, .. } = &first else {
            panic!("the run did not complete: {first:?}");
        };
        assert!(
            tasks_run.iter().any(|t| t == "process_queues:TIMEOUT"),
            "process_queues was waiting on a queue that never answers, so it must \
             be recorded as timed out: {tasks_run:?}"
        );
        assert!(
            tasks_run
                .iter()
                .any(|t| t.starts_with("cleanup_temp_files")),
            "the tasks before the stuck one still ran: {tasks_run:?}"
        );

        // Released the moment the run returned, not left for the TTL.
        assert_eq!(
            lock_holder().await,
            None,
            "a run cut short by a task timeout must give the lock back at once"
        );

        let second = tokio::time::timeout(Duration::from_secs(120), cron.run())
            .await
            .expect("the second run never came back");
        assert!(
            matches!(second, CronResult::Completed { .. }),
            "the next run must go ahead, not skip behind the stuck one: {second:?}"
        );
        assert_eq!(lock_holder().await, None);
    });
}

/// A `MakeWriter` that keeps everything logged, for the assertions below.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Plant a lock held by another run since `held_secs` ago, run cron, and
/// return the result and what it logged.
async fn skip_behind(held_secs: i64) -> (CronResult, String) {
    let app = shared_app().await;
    let since = chrono::Utc::now().timestamp() - held_secs;
    let holder = format!("elsewhere:4242:t{since}");
    let _: () = redis::cmd("SET")
        .arg(CRON_LOCK_KEY)
        .arg(&holder)
        .arg("EX")
        .arg(300)
        .query_async(&mut redis_conn().await)
        .await
        .unwrap();

    let cron = CronService::new(
        redis::Client::open(trovato_test_utils::env::redis_url()).unwrap(),
        app.db.clone(),
    );

    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    // `run_test` polls the body on this thread, and the skip is logged from
    // the run's own future rather than a spawned task, so a thread default
    // sees it.
    let result = {
        let _default = tracing::subscriber::set_default(subscriber);
        cron.run().await
    };
    let logged = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    (result, logged)
}

/// A lock held far longer than any run should take is a stuck holder, and the
/// skip says so at warn, with the age. Before, every skip was one `debug!`
/// line, which production does not show: ten hours of skipped ticks looked
/// like a quiet cron.
#[test]
fn a_skip_behind_a_lock_held_too_long_warns_with_its_age() {
    run_test(async {
        // Held until the cleanups below have run: released at the end of the body,
        // this test's `DEL cron:lock` would land in the middle of the next test.
        common::hold_until_cleanup(CRON_LOCK.lock().await);
        defer_clear_lock();

        let (result, logged) = skip_behind(2_000).await;
        eprintln!("{logged}");
        assert!(matches!(result, CronResult::Skipped), "{result:?}");
        let warned = logged
            .lines()
            .find(|l| l.contains("WARN") && l.contains("cron lock has been held for"))
            .unwrap_or_else(|| panic!("no warn about the old lock in:\n{logged}"));
        assert!(
            warned.contains("held_secs=2000") || warned.contains("held_secs=2001"),
            "the warn must name the lock's age: {warned}"
        );
    });
}

/// An ordinary overlap, another run a few seconds in, stays at debug.
#[test]
fn a_skip_behind_a_fresh_lock_stays_at_debug() {
    run_test(async {
        // Held until the cleanups below have run: released at the end of the body,
        // this test's `DEL cron:lock` would land in the middle of the next test.
        common::hold_until_cleanup(CRON_LOCK.lock().await);
        defer_clear_lock();

        let (result, logged) = skip_behind(5).await;
        eprintln!("{logged}");
        assert!(matches!(result, CronResult::Skipped), "{result:?}");
        assert!(
            !logged.contains("WARN"),
            "an ordinary overlap is not worth a warn:\n{logged}"
        );
        assert!(
            logged.contains("DEBUG") && logged.contains("cron lock held by another instance"),
            "the overlap is still logged at debug:\n{logged}"
        );
    });
}
