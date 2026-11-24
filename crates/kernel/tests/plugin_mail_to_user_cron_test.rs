#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `mail.send-to-user` sends from `tap_cron` (**BL-94**, with BL-72).
//!
//! Member mail is what a digest sends, and a digest is sent from cron. This
//! wires a `CronService` the way `AppState` does, with the email service
//! pointed at a loopback sink, names a member in the fixture's
//! `mail_user_target` variable, runs the cron cycle, and reads the sink.
//!
//! Runs the **real** `plugins/test_queue_worker` wasm, which declares
//! `mail_users`. Requires Postgres + Redis and the fixture `.wasm`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};

use common::smtp_sink::SmtpSink;
use sqlx::PgPool;
use trovato_kernel::config::MailConfig;
use trovato_kernel::cron::{CronResult, CronService};
use trovato_kernel::middleware::{RateLimitConfig, RateLimiter};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::email::EmailService;
use trovato_kernel::services::mail_transport::SmtpTransport;
use trovato_kernel::services::mail_worker::{DrainScope, Lane, MailWorker};
use trovato_kernel::tap::{TapDispatcher, TapRegistry};
use uuid::Uuid;

const FIXTURE: &str = "test_queue_worker";
const TARGET_KEY: &str = "plugin.test_queue_worker.mail_user_target";

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});

static SINK: OnceLock<SmtpSink> = OnceLock::new();

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

fn dispatcher() -> Arc<TapDispatcher> {
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&plugins_dir().join(FIXTURE))
        .unwrap_or_else(|e| panic!("failed to load fixture '{FIXTURE}': {e:#}"));
    let runtime = Arc::new(runtime);
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    Arc::new(TapDispatcher::new(runtime, registry))
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

#[test]
fn tap_cron_mails_a_member() {
    let sink = SINK.get_or_init(|| RT.block_on(SmtpSink::start()));
    RT.block_on(async {
        let pool = fresh_pool().await;
        let user = Uuid::now_v7();
        let name = format!("bl94cron_{}", user.simple());
        let address = format!("{name}@member.bl94.example.test");
        sqlx::query("INSERT INTO users (id, name, pass, mail) VALUES ($1, $2, 'x', $3)")
            .bind(user)
            .bind(&name)
            .bind(&address)
            .execute(&pool)
            .await
            .unwrap();
        SiteConfig::set(&pool, TARGET_KEY, serde_json::json!(user.to_string()))
            .await
            .unwrap();

        let limiter = Arc::new(RateLimiter::new(
            redis_client(),
            RateLimitConfig::default(),
            Vec::new(),
        ));
        limiter
            .reset("mail_users", &format!("plugin:{FIXTURE}"))
            .await
            .ok();
        let transport =
            SmtpTransport::new("127.0.0.1", sink.port, None, None, "none").expect("transport");
        let email = Arc::new(EmailService::new(
            pool.clone(),
            Arc::new(transport),
            "no-reply@bl94.example.test".to_string(),
            "http://localhost:3000".to_string(),
            MailConfig::default(),
        ));
        let mut cron = CronService::new(redis_client(), pool.clone());
        cron.set_tap_dispatcher(dispatcher());
        cron.set_rate_limiter(limiter);
        cron.set_email_service(Some(Arc::clone(&email)));

        let mut ran = false;
        for _ in 0..60 {
            if !matches!(cron.run().await, CronResult::Skipped) {
                ran = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // The plugin's call queued the message (BG18); send what is queued
        // for this member.
        MailWorker::new(
            &email,
            Lane::Any,
            DrainScope::Recipients(vec![address.clone()]),
        )
        .drain()
        .await
        .expect("drain the outbox");

        let delivered: Vec<_> = sink
            .messages()
            .into_iter()
            .filter(|m| m.recipients.iter().any(|r| r == &address))
            .collect();

        SiteConfig::delete(&pool, TARGET_KEY).await.ok();
        sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
            .bind(FIXTURE)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .ok();

        assert!(ran, "the cron cycle never acquired its lock");
        assert_eq!(delivered.len(), 1, "tap_cron must mail the member once");
        assert!(delivered[0].data.contains("Subject: cron member mail"));
        assert!(delivered[0].data.contains("/user/mail/unsubscribe/"));
    });
}
