#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Plugin mail sends from `tap_cron` and `tap_queue_worker` (**BL-72**,
//! G-MAIL-UNAVAILABLE-IN-BACKGROUND).
//!
//! Background dispatch used to carry no `EmailService`, so
//! `send-to-site-contacts` from cron or a queue worker always returned
//! `ERR_MAIL_NOT_CONFIGURED` on a site that had SMTP configured, although
//! `CronService` held the same service for its own queue. These tests wire a
//! `CronService` the way `AppState` does, with the email service pointed at a
//! loopback SMTP sink, and prove both paths deliver and both are counted
//! against the per-plugin `mail` bucket.
//!
//! Everything runs the **real** `plugins/test_queue_worker` wasm through the
//! **real** `CronService`. Requires Postgres + Redis and the fixture `.wasm`
//! built into `plugins/test_queue_worker/`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use common::smtp_sink::{Envelope, SmtpSink};
use sqlx::PgPool;
use trovato_kernel::cron::{CronResult, CronService};
use trovato_kernel::middleware::{RateLimitConfig, RateLimiter};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::email::EmailService;
use trovato_kernel::tap::{TapDispatcher, TapRegistry};

const FIXTURE: &str = "test_queue_worker";

/// Where the site says its contact address is. The plugin never names it.
const SITE_CONTACT: &str = "contact@bl72.example.test";
const SITE_FROM: &str = "no-reply@bl72.example.test";

fn bucket_identifier() -> String {
    format!("plugin:{FIXTURE}")
}

/// Serializes these tests: one Redis bucket, one queue, one `site_mail` key.
static SERIAL: Mutex<()> = Mutex::new(());

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});

fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    // Started outside `block_on`: starting it is itself a `block_on`.
    sink();
    RT.block_on(body);
}

static FIXTURE_DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

/// One sink for the binary, accepting on the binary's runtime.
static SINK: OnceLock<SmtpSink> = OnceLock::new();

fn sink() -> &'static SmtpSink {
    SINK.get_or_init(|| RT.block_on(SmtpSink::start()))
}

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
    redis::Client::open(trovato_test_utils::env::redis_url()).expect("redis client")
}

/// A `CronService` wired the way `AppState` wires it: the fixture dispatcher,
/// the rate limiter, and the one email service, here pointed at the sink.
fn cron(pool: PgPool) -> (Arc<CronService>, Arc<RateLimiter>) {
    let limiter = Arc::new(RateLimiter::new(
        redis_client(),
        RateLimitConfig {
            mail: (100, std::time::Duration::from_secs(60)),
            ..Default::default()
        },
        Vec::new(),
    ));
    let email = EmailService::new(
        "127.0.0.1",
        sink().port,
        None,
        None,
        "none",
        SITE_FROM.to_string(),
        "http://localhost:3000".to_string(),
    )
    .expect("email service");
    let mut cron = CronService::new(redis_client(), pool);
    cron.set_tap_dispatcher(dispatcher());
    cron.set_rate_limiter(limiter.clone());
    cron.set_email_service(Some(Arc::new(email)));
    (Arc::new(cron), limiter)
}

async fn clean_queue(pool: &PgPool) {
    sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
        .bind(FIXTURE)
        .execute(pool)
        .await
        .unwrap();
}

async fn remaining(pool: &PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_queue WHERE plugin_name = $1")
        .bind(FIXTURE)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn messages_for(subject: &str) -> Vec<Envelope> {
    sink()
        .messages()
        .into_iter()
        .filter(|m| m.data.contains(&format!("Subject: {subject}")))
        .collect()
}

/// Point `site_mail` at the test contact, returning what it was.
async fn set_contact(pool: &PgPool) -> String {
    let previous = SiteConfig::site_mail(pool).await.unwrap_or_default();
    SiteConfig::set_site_mail(pool, SITE_CONTACT).await.unwrap();
    previous
}

/// A queue worker's `send-to-site-contacts` is delivered, and counted against
/// the plugin's `mail` bucket.
#[test]
fn a_queue_worker_sends_mail_and_spends_the_bucket() {
    serial(async {
        let pool = fresh_pool().await;
        let (cron, limiter) = cron(pool.clone());
        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;
        let previous = set_contact(&pool).await;
        let before = messages_for("queue worker mail").len();

        // The fixture traps unless the host answers 0, so a surviving row is
        // the send having been refused.
        sqlx::query(
            r#"
            INSERT INTO plugin_queue
                (plugin_name, queue_name, payload, created_at, priority, max_attempts,
                 next_attempt_at, status, attempts, locked_until)
            VALUES ($1, 'test_queue', $2, 0, 0, 1, 0, 'ready', 0, 0)
            "#,
        )
        .bind(FIXTURE)
        .bind(serde_json::json!({ "outcome": "mail", "expect_code": 0 }))
        .execute(&pool)
        .await
        .unwrap();
        cron.drain_plugin_queues().await.unwrap();

        let left = remaining(&pool).await;
        let sent = messages_for("queue worker mail");
        let count = limiter
            .get_count("mail", &bucket_identifier())
            .await
            .unwrap();
        SiteConfig::set_site_mail(&pool, &previous).await.ok();
        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;

        assert_eq!(
            left, 0,
            "the worker was refused; a surviving row is its assertion having trapped"
        );
        assert_eq!(sent.len(), before + 1, "one mail must reach the sink");
        let message = sent.last().unwrap();
        assert_eq!(message.recipients, vec![SITE_CONTACT.to_string()]);
        assert!(message.data.contains(&format!("From: {SITE_FROM}")));
        assert_eq!(count, 1, "the send must be counted against the mail bucket");
    });
}

/// `tap_cron`'s `send-to-site-contacts` is delivered, and counted against the
/// plugin's `mail` bucket.
#[test]
fn tap_cron_sends_mail_and_spends_the_bucket() {
    serial(async {
        let pool = fresh_pool().await;
        let (cron, limiter) = cron(pool.clone());
        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;
        let previous = set_contact(&pool).await;
        let before = messages_for("cron tap mail").len();

        // `run` takes the shared cron lock; another binary's cycle may hold it
        // briefly, so wait for it rather than pass silently.
        let mut ran = false;
        for _ in 0..60 {
            if !matches!(cron.run().await, CronResult::Skipped) {
                ran = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        let sent = messages_for("cron tap mail");
        let count = limiter
            .get_count("mail", &bucket_identifier())
            .await
            .unwrap();
        SiteConfig::set_site_mail(&pool, &previous).await.ok();
        limiter.reset("mail", &bucket_identifier()).await.unwrap();
        clean_queue(&pool).await;

        assert!(ran, "the cron cycle never acquired its lock");
        assert_eq!(
            sent.len(),
            before + 1,
            "tap_cron's mail must reach the sink"
        );
        let message = sent.last().unwrap();
        assert_eq!(message.recipients, vec![SITE_CONTACT.to_string()]);
        assert!(message.data.contains("sent from tap_cron"));
        assert_eq!(
            count, 1,
            "tap_cron's send must be counted against the mail bucket"
        );
    });
}
