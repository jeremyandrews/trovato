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

/// An item `process_queues` has already popped is handled even when the task
/// is cut off while handling it.
///
/// `LPOP` removes the item from Redis, so before each item was handled on its
/// own task, a timeout landing while the handler waited on the database dropped
/// the item for good: nothing left in Redis, nothing claimed to retry. Here the
/// reindex handler is kept waiting for a database connection past the task's
/// bound, the connection is then let go, and the item must still be reindexed.
#[test]
fn an_item_popped_before_a_task_timeout_is_still_handled() {
    use trovato_kernel::cron::{CronTasks, Queue, RedisQueue};

    run_test(async {
        common::hold_until_cleanup(CRON_LOCK.lock().await);
        let app = shared_app().await;

        let item_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO item (id, type, title, author_id, status, created, changed, promote, \
             sticky, fields, language) \
             VALUES ($1, 'page', 'B247 reindex', $2, 1, 1, 1, 0, 0, '{}'::jsonb, 'en')",
        )
        .bind(item_id)
        .bind(uuid::Uuid::nil())
        .execute(&app.db)
        .await
        .unwrap();
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
        });

        let redis = redis::Client::open(trovato_test_utils::env::redis_url()).unwrap();
        let queue = Arc::new(RedisQueue::new(redis));
        // Nothing else in this binary uses these queues; start them empty.
        for key in ["queue:email:send", "queue:search:reindex"] {
            let _: () = redis::cmd("DEL")
                .arg(key)
                .query_async(&mut redis_conn().await)
                .await
                .unwrap();
        }
        queue
            .push("search:reindex", &item_id.to_string())
            .await
            .unwrap();
        // The tasks get a pool of one connection, and the test holds it, so the
        // handler waits for a connection before its UPDATE is ever sent. (A
        // statement already sent would finish on the server whatever became of
        // the future that sent it, so a lock on the row would prove nothing.)
        let one = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(60))
            .connect(&trovato_test_utils::env::database_url())
            .await
            .unwrap();
        let tasks = CronTasks::new(one.clone(), queue.clone());
        let blocker = one.acquire().await.unwrap();

        let cut_off = tokio::time::timeout(Duration::from_secs(1), tasks.process_queues()).await;
        assert!(
            cut_off.is_err(),
            "the handler was meant to be waiting for a connection: {cut_off:?}"
        );
        assert_eq!(
            queue.len("search:reindex").await.unwrap(),
            0,
            "the item was popped before the task was cut off"
        );

        drop(blocker);

        let mut changed = 1;
        for _ in 0..100 {
            changed = sqlx::query_scalar::<_, i64>("SELECT changed FROM item WHERE id = $1")
                .bind(item_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
            if changed > 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            changed > 1,
            "the popped item was never reindexed: a task timeout dropped it, and with \
             it the only copy of the request"
        );
    });
}

/// A cron run whose database read of the queue-runner setting cannot finish
/// times that read out, carries on, and gives the lock back.
///
/// The read sat outside every task bound, so a stalled database held the run,
/// and with it the cron lock, for as long as the stall lasted.
#[test]
fn a_stalled_queue_runner_config_read_is_timed_out() {
    run_test(async {
        common::hold_until_cleanup(CRON_LOCK.lock().await);
        let app = shared_app().await;
        clear_lock().await;
        defer_clear_lock();

        let mut cron = CronService::new(
            redis::Client::open(trovato_test_utils::env::redis_url()).unwrap(),
            app.db.clone(),
        );
        cron.set_task_timeout(Duration::from_secs(1));

        // Nothing can read site_config while this is held.
        let mut blocker = app.db.begin().await.unwrap();
        sqlx::query("LOCK TABLE site_config IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();

        let result = tokio::time::timeout(Duration::from_secs(120), cron.run()).await;
        let holder = lock_holder().await;
        blocker.rollback().await.unwrap();

        let result = result.expect("the run never came back: a stalled read holds the whole run");
        let CronResult::Completed { tasks_run, .. } = &result else {
            panic!("the run did not complete: {result:?}");
        };
        assert!(
            tasks_run.iter().any(|t| t == "queue_runner_config:TIMEOUT"),
            "the stalled read must be recorded as timed out: {tasks_run:?}"
        );
        assert_eq!(holder, None, "the run must give the lock back at once");
    });
}
