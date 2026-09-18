#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Plugin mail is bounded on the **queue-worker** path (the `mail` bucket).
//!
//! The web-facing case was already covered: a plugin-served POST falls into the
//! `forms` rate-limit bucket per client IP like any other form post. A plugin
//! calling `mail` from `tap_cron` or `tap_queue_worker` fell into nothing at all,
//! because the rate limiter only ever saw requests and background dispatch never
//! carried it.
//!
//! The bucket is now checked inside the host function, keyed by plugin, so it
//! applies wherever the call comes from. It is checked **first**, before the
//! SMTP-handle test and before payload validation: otherwise a background caller
//! would be told "no SMTP host" forever and never reach its own limit, and a
//! plugin could spend its window on malformed requests.
//!
//! Everything here runs the **real** `plugins/test_queue_worker` wasm through the
//! **real** `CronService` drain. The fixture's `"mail"` payload states the host
//! code it expects and traps on any other, which is how the answer becomes
//! observable: the drain deletes a succeeding job and keeps no record of what it
//! returned, so a surviving row *is* the failed assertion.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_queue_worker/`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::PgPool;
use trovato_kernel::cron::CronService;
use trovato_kernel::middleware::{RateLimitConfig, RateLimiter};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{TapDispatcher, TapRegistry};
use trovato_sdk::host_errors;

/// The fixture plugin name, which is also its rate-limit identity.
const FIXTURE: &str = "test_queue_worker";

/// The bucket key the host function uses: per plugin, not per client.
fn bucket_identifier() -> String {
    format!("plugin:{FIXTURE}")
}

/// Serializes these tests: they share one Redis bucket and one queue.
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

/// One dispatcher for the binary: a fresh `PluginRuntime` reserves a large
/// pooling-allocator slab.
static FIXTURE_DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

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
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    redis::Client::open(url).expect("redis client")
}

/// A limiter with the mail bucket set to `limit` a minute.
///
/// A minute rather than the shipped hour so a leftover key cannot poison the
/// next run for an hour; every test resets its bucket anyway.
fn limiter_with_mail_limit(limit: u32) -> Arc<RateLimiter> {
    let config = RateLimitConfig {
        mail: (limit, std::time::Duration::from_secs(60)),
        ..Default::default()
    };
    Arc::new(RateLimiter::new(redis_client(), config, Vec::new()))
}

/// A `CronService` wired to the fixture dispatcher, with the mail bucket armed
/// exactly as `AppState` arms it in production.
fn cron_with_mail_limit(pool: PgPool, limit: u32) -> (Arc<CronService>, Arc<RateLimiter>) {
    let limiter = limiter_with_mail_limit(limit);
    let mut cron = CronService::new(redis_client(), pool);
    cron.set_tap_dispatcher(dispatcher());
    cron.set_rate_limiter(limiter.clone());
    (Arc::new(cron), limiter)
}

async fn clean_queue(pool: &PgPool) {
    sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
        .bind(FIXTURE)
        .execute(pool)
        .await
        .unwrap();
}

/// Seed one `"mail"` job that asserts the host returns `expect_code`.
async fn seed_mail_job(pool: &PgPool, expect_code: i32) {
    sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, max_attempts,
             next_attempt_at, status, attempts, locked_until)
        VALUES ($1, 'test_queue', $2, 0, 0, 1, 0, 'ready', 0, 0)
        "#,
    )
    .bind(FIXTURE)
    .bind(serde_json::json!({ "outcome": "mail", "expect_code": expect_code }))
    .execute(pool)
    .await
    .unwrap();
}

/// Rows left for the fixture. Zero means every job's assertion held.
async fn remaining(pool: &PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_queue WHERE plugin_name = $1")
        .bind(FIXTURE)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The defect, at the layer it lives: a plugin calling `mail` from a queue worker
/// past its bucket is refused, and the refusal says which refusal it is.
#[test]
fn a_queue_worker_past_the_mail_limit_is_refused() {
    serial(async {
        let pool = fresh_pool().await;
        let (cron, limiter) = cron_with_mail_limit(pool.clone(), 1);
        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;

        // Spend the bucket's one allowance, the way an earlier send would.
        limiter
            .check("mail", &bucket_identifier())
            .await
            .expect("the first send is within the limit");

        // The worker now asserts it is told it was rate limited, and traps if it
        // is told anything else — including the "no SMTP host" answer it would
        // get if the bucket were checked after the email handle.
        seed_mail_job(&pool, host_errors::ERR_MAIL_RATE_LIMITED).await;
        cron.drain_plugin_queues().await.unwrap();

        assert_eq!(
            remaining(&pool).await,
            0,
            "the worker was not told it was rate limited; a surviving row is the \
             fixture's assertion having trapped"
        );

        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;
    });
}

/// Within the bucket, the call is not refused *by the bucket*. Background
/// dispatch carries no email handle (BL-72), so it is still refused — but for the
/// pre-existing reason, which is what proves the limiter is not refusing
/// everything unconditionally.
#[test]
fn a_queue_worker_within_the_mail_limit_reaches_past_the_bucket() {
    serial(async {
        let pool = fresh_pool().await;
        let (cron, limiter) = cron_with_mail_limit(pool.clone(), 10);
        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;

        seed_mail_job(&pool, host_errors::ERR_MAIL_NOT_CONFIGURED).await;
        cron.drain_plugin_queues().await.unwrap();

        assert_eq!(
            remaining(&pool).await,
            0,
            "within the bucket the call must get past it and be answered by the \
             next check, not refused as rate limited"
        );

        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;
    });
}

/// Nothing is ever sent on this path: background dispatch carries no
/// `EmailService`, so the refusal is a refusal, not a delivery. Pins the
/// interaction with BL-72 — when background mail is enabled, this assertion is
/// what says the bucket is the thing standing between a loop and the mailbox.
#[test]
fn the_bucket_is_consumed_by_the_attempt_itself() {
    serial(async {
        let pool = fresh_pool().await;
        let (cron, limiter) = cron_with_mail_limit(pool.clone(), 2);
        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;

        // Two jobs, both within the bucket, both answered past it.
        seed_mail_job(&pool, host_errors::ERR_MAIL_NOT_CONFIGURED).await;
        seed_mail_job(&pool, host_errors::ERR_MAIL_NOT_CONFIGURED).await;
        cron.drain_plugin_queues().await.unwrap();
        assert_eq!(remaining(&pool).await, 0, "both were within the bucket");

        // The bucket counted them: a third attempt is refused. An attempt that
        // could not be delivered still spends the allowance, which is the only
        // way a limit can bound a loop that never succeeds.
        assert_eq!(
            limiter
                .get_count("mail", &bucket_identifier())
                .await
                .unwrap(),
            2,
            "each attempt counts, delivered or not"
        );

        seed_mail_job(&pool, host_errors::ERR_MAIL_RATE_LIMITED).await;
        cron.drain_plugin_queues().await.unwrap();
        assert_eq!(
            remaining(&pool).await,
            0,
            "the third was refused by the bucket"
        );

        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;
    });
}

/// The bucket is per plugin, so one plugin exhausting it cannot silence another.
#[test]
fn the_mail_bucket_is_keyed_per_plugin() {
    serial(async {
        let limiter = limiter_with_mail_limit(1);
        let mine = format!("plugin:{FIXTURE}");
        let theirs = "plugin:some_other_plugin".to_string();
        limiter.reset("mail", &mine).await.unwrap();
        limiter.reset("mail", &theirs).await.unwrap();

        limiter
            .check("mail", &mine)
            .await
            .expect("first is allowed");
        limiter
            .check("mail", &mine)
            .await
            .expect_err("the second exhausts this plugin's bucket");
        limiter
            .check("mail", &theirs)
            .await
            .expect("another plugin has its own bucket");

        limiter.reset("mail", &mine).await.unwrap();
        limiter.reset("mail", &theirs).await.unwrap();
    });
}
