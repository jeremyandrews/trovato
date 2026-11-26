#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A Redis that stops answering must not hang the kernel (B247).
//!
//! In the incident behind this file a SYN flood cut a pod off from Redis for a
//! minute or two. The cron run that had just started sat in its queue pop for
//! ten hours: neither the connection nor the command had a timeout, the TCP
//! connection never closed, so the await never returned.
//!
//! Each test points the kernel at a fake Redis on a local port. The fake
//! accepts the connection, answers the client's connection setup so the
//! failure lands on a real command, and then does one of three things:
//!
//! - [`Mode::Silent`]: answers nothing at all, not even the setup, so the
//!   *connection* never completes;
//! - [`Mode::SilentOnCommand`]: holds the socket open and never answers the
//!   command, which is what a peer behind a black-holed route looks like;
//! - [`Mode::ResetOnCommand`]: resets the connection mid-command.
//!
//! Every call must come back, with an error, inside [`BOUND`]. Before the
//! kernel gave its Redis connections timeouts the first two never came back
//! at all. Each test also checks that the process burns no CPU while the call
//! is stuck: the incident's worker threads sat at a full core for ten hours.

use std::sync::Arc;
use std::time::{Duration, Instant};

use trovato_kernel::cron::{CronResult, CronService, Queue, RedisQueue};

/// How long any one stuck call may take to come back. Several times the
/// kernel's Redis timeouts, so a slow CI machine does not make it flaky, and
/// far short of "forever", which is what it was.
const BOUND: Duration = Duration::from_secs(30);

/// CPU the whole test process may spend while one call is stuck. An idle
/// await costs microseconds; a busy loop costs a second per second.
const CPU_ALLOWANCE: Duration = Duration::from_millis(1500);

/// One test at a time, so the CPU measurement is this test's own.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[path = "common/fake_redis.rs"]
mod fake_redis;

use fake_redis::{Mode, cpu_time, fake_redis};

/// Run `fut` under [`BOUND`] and check it came back, and cheaply.
async fn must_return<T: std::fmt::Debug>(
    what: &str,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let started = Instant::now();
    let cpu_before = cpu_time();
    let outcome = tokio::time::timeout(BOUND, fut).await;
    let elapsed = started.elapsed();
    let cpu = cpu_time().saturating_sub(cpu_before);
    eprintln!(
        "{what}: returned={} after {elapsed:?}, cpu {cpu:?}",
        outcome.is_ok()
    );
    let Ok(value) = outcome else {
        panic!(
            "{what} was still waiting on Redis after {BOUND:?}: a Redis that stops \
             answering hangs the caller for ever (cpu spent while stuck: {cpu:?})"
        );
    };
    assert!(
        cpu < CPU_ALLOWANCE,
        "{what} spent {cpu:?} of CPU in {elapsed:?} while Redis was not answering: \
         something is spinning"
    );
    value
}

async fn queue_pop(mode: Mode, timeout_secs: u64) {
    let _serial = SERIAL.lock().await;
    let url = fake_redis(mode).await;
    let queue = RedisQueue::new(redis::Client::open(url.as_str()).unwrap());
    let what = format!("RedisQueue::pop(timeout_secs = {timeout_secs}) against {mode:?}");
    let result = must_return(&what, queue.pop("b247:stuck", timeout_secs)).await;
    assert!(
        result.is_err(),
        "{what} returned {result:?}: a Redis that never answered cannot have produced it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pop_from_a_redis_that_never_connects_comes_back() {
    queue_pop(Mode::Silent, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pop_from_a_silent_redis_comes_back() {
    queue_pop(Mode::SilentOnCommand, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocking_pop_from_a_silent_redis_comes_back() {
    queue_pop(Mode::SilentOnCommand, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pop_from_a_redis_that_resets_comes_back() {
    queue_pop(Mode::ResetOnCommand, 0).await;
}

/// A whole cron run against a Redis that stops answering must end, not hang:
/// the incident's run never returned, and the next ten hours of cron ticks were
/// all skipped behind it.
async fn cron_run(mode: Mode) {
    let _serial = SERIAL.lock().await;
    let url = fake_redis(mode).await;
    // The run must give up on the lock before it ever reaches the database,
    // so the pool is lazy and never connects.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&trovato_test_utils::env::database_url())
        .unwrap();
    let cron = Arc::new(CronService::new(
        redis::Client::open(url.as_str()).unwrap(),
        pool,
    ));
    let what = format!("CronService::run against {mode:?}");
    let result = must_return(&what, cron.run()).await;
    assert!(
        matches!(result, CronResult::Failed(_)),
        "{what} returned {result:?}: with no Redis there is no lock, so the run must fail"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cron_run_against_a_redis_that_never_connects_comes_back() {
    cron_run(Mode::Silent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cron_run_against_a_silent_redis_comes_back() {
    cron_run(Mode::SilentOnCommand).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cron_run_against_a_redis_that_resets_comes_back() {
    cron_run(Mode::ResetOnCommand).await;
}
