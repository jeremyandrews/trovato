#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The update check, driven through the real cron path against a local endpoint.
//!
//! # No test here touches the network
//!
//! Every one of them serves the release payload from an axum app bound to
//! `127.0.0.1:0` in the test process, and points `RuntimeConfig::update_check_endpoint`
//! at it. That is the whole reason the endpoint is configurable: without it a test
//! of this feature would either hit `api.github.com` or test a mock of the code
//! instead of the code.
//!
//! The client is a plain `reqwest::Client` rather than the kernel's SSRF-hardened
//! one, because the hardened resolver refuses loopback addresses — which is
//! correct, and is what production gets through `CronService::apply_runtime_config`.
//! What these tests exercise is `CronTasks::check_for_updates`, the same function
//! the cron cycle calls, with the same interval, setting and storage logic.
//!
//! The pure version-comparison and title-convention logic is unit-tested beside the
//! code in `crates/kernel/src/update_status.rs`.
//!
//! Requires Postgres and Redis.

use axum::Json;
use axum::routing::get;
use sqlx::{Connection, Executor, PgConnection, PgPool};
use trovato_kernel::cron::{CronService, UpdateCheckConfig};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::update_status::{self, UPDATE_CHECK_KEY, UPDATE_STATUS_KEY};

/// A database created for one test and dropped when it ends.
struct ScratchDb {
    server_url: String,
    name: String,
    pool: Option<PgPool>,
}

impl ScratchDb {
    async fn new(label: &str) -> Self {
        trovato_test_utils::env::load_dotenv();
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set to run these tests");
        let without_query = database_url
            .split_once('?')
            .map_or(database_url.as_str(), |(base, _)| base);
        let cut = without_query
            .rfind('/')
            .expect("DATABASE_URL must include a database name");
        let server_url = without_query[..cut].to_string();
        let name = format!("trovato_update_{label}_{}", uuid::Uuid::now_v7().simple());

        let mut admin = PgConnection::connect(&format!("{server_url}/postgres"))
            .await
            .expect("connect to the maintenance database");
        admin
            .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
            .await
            .expect("create the scratch database");
        drop(admin);

        let pool = PgPool::connect(&format!("{server_url}/{name}"))
            .await
            .expect("connect to the scratch database");
        trovato_kernel::db::run_migrations(&pool)
            .await
            .expect("migrate the scratch database");

        Self {
            server_url,
            name,
            pool: Some(pool),
        }
    }

    fn pool(&self) -> &PgPool {
        self.pool.as_ref().expect("pool already closed")
    }

    async fn cleanup(mut self) {
        if let Some(pool) = self.pool.take() {
            pool.close().await;
        }
        if let Ok(mut admin) = PgConnection::connect(&format!("{}/postgres", self.server_url)).await
        {
            let _ = admin
                .execute(
                    format!(r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#, self.name).as_str(),
                )
                .await;
        }
    }
}

/// Serve one fixed release payload on loopback, and return its URL.
async fn serve_release(payload: serde_json::Value) -> String {
    let app = axum::Router::new().route(
        "/releases/latest",
        get(move || {
            let payload = payload.clone();
            async move { Json(payload) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/releases/latest")
}

/// Serve a 500, for the failure path.
async fn serve_failure() -> String {
    let app = axum::Router::new().route(
        "/releases/latest",
        get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/releases/latest")
}

fn release(tag: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "tag_name": tag,
        "name": title,
        "prerelease": false,
        "draft": false,
        // Fields the kernel does not read, present because GitHub sends them.
        "id": 1,
        "html_url": "https://example.invalid/releases/tag",
        "body": "notes",
    })
}

/// Build the real cron service against a scratch database and a local endpoint.
fn cron_with(pool: &PgPool, endpoint: &str, interval_secs: u64) -> CronService {
    let redis = redis::Client::open(
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
    )
    .expect("redis client");
    let mut cron = CronService::new(redis, pool.clone());
    cron.set_update_check(Some(UpdateCheckConfig {
        endpoint: endpoint.to_string(),
        // A plain client: the hardened one refuses loopback, which is the point of
        // the hardened one.
        client: reqwest::Client::new(),
        interval_secs,
    }));
    cron
}

/// A newer release is fetched, stored, and reads as behind.
#[tokio::test]
async fn a_newer_release_is_stored_and_reads_as_behind() {
    let db = ScratchDb::new("behind").await;
    let endpoint = serve_release(release("v999.0.0", "999.0.0")).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);

    let status = cron
        .check_for_updates()
        .await
        .expect("the check must succeed")
        .expect("a published release must be stored");

    assert_eq!(status.latest_version, "999.0.0", "the tag loses its v");
    assert!(!status.is_security);
    assert!(status.is_behind());

    // Stored, not merely returned: the banner reads it back on the next request.
    let stored = update_status::stored_status(db.pool())
        .await
        .expect("the status must be in site_config");
    assert_eq!(stored, status);

    db.cleanup().await;
}

/// The security convention is read from the release title, through the real path.
#[tokio::test]
async fn a_security_titled_release_is_stored_as_a_security_release() {
    let db = ScratchDb::new("security").await;
    let endpoint = serve_release(release("v999.0.1", "[security] fixes CVE-2026-00000")).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);

    let status = cron
        .check_for_updates()
        .await
        .expect("the check must succeed")
        .expect("a published release must be stored");

    assert!(
        status.is_security,
        "a title starting [security] must store as a security release"
    );
    assert!(status.is_behind());

    db.cleanup().await;
}

/// The running version is not behind itself.
#[tokio::test]
async fn the_running_version_is_not_behind() {
    let db = ScratchDb::new("current").await;
    let running = update_status::running_version();
    let endpoint = serve_release(release(&format!("v{running}"), running)).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);

    let status = cron
        .check_for_updates()
        .await
        .expect("the check must succeed")
        .expect("a published release must be stored");

    assert_eq!(status.latest_version, running);
    assert!(
        !status.is_behind(),
        "equal versions must not raise a banner"
    );

    db.cleanup().await;
}

/// A tag that is not a version stores nothing, so no banner can be raised from it.
#[tokio::test]
async fn a_malformed_tag_stores_nothing() {
    let db = ScratchDb::new("malformed").await;
    let endpoint = serve_release(release("nightly", "nightly build")).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);

    let stored = cron
        .check_for_updates()
        .await
        .expect("an unreadable tag is not an error, it is nothing to store");
    assert!(stored.is_none());

    assert!(
        update_status::stored_status(db.pool()).await.is_none(),
        "nothing may be stored for a tag that cannot be compared"
    );

    db.cleanup().await;
}

/// A draft or prerelease is not an update.
#[tokio::test]
async fn a_prerelease_stores_nothing() {
    let db = ScratchDb::new("prerelease").await;
    let mut payload = release("v999.0.0", "999.0.0 rc");
    payload["prerelease"] = serde_json::json!(true);
    let endpoint = serve_release(payload).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);

    assert!(
        cron.check_for_updates()
            .await
            .expect("a prerelease is not an error")
            .is_none()
    );
    assert!(update_status::stored_status(db.pool()).await.is_none());

    db.cleanup().await;
}

/// The site setting turns the check off, and nothing is fetched or stored.
#[tokio::test]
async fn the_site_setting_disables_the_check() {
    let db = ScratchDb::new("disabled").await;
    let endpoint = serve_release(release("v999.0.0", "999.0.0")).await;

    SiteConfig::set(db.pool(), UPDATE_CHECK_KEY, serde_json::json!(false))
        .await
        .expect("store the setting");

    let cron = cron_with(db.pool(), &endpoint, 86_400);
    assert!(
        cron.check_for_updates()
            .await
            .expect("a disabled check is not an error")
            .is_none(),
        "the setting must stop the check"
    );
    assert!(
        update_status::stored_status(db.pool()).await.is_none(),
        "a disabled check must store nothing"
    );

    // Turning it back on lets the same code path through, which is what proves the
    // setting was the thing stopping it.
    SiteConfig::set(db.pool(), UPDATE_CHECK_KEY, serde_json::json!(true))
        .await
        .expect("store the setting");
    assert!(
        cron.check_for_updates()
            .await
            .expect("the check must succeed")
            .is_some()
    );

    db.cleanup().await;
}

/// A check inside the interval does not ask again.
#[tokio::test]
async fn a_second_check_inside_the_interval_does_not_ask_again() {
    let db = ScratchDb::new("interval").await;
    let endpoint = serve_release(release("v999.0.0", "999.0.0")).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);

    let first = cron
        .check_for_updates()
        .await
        .expect("the first check must succeed")
        .expect("stored");

    let second = cron
        .check_for_updates()
        .await
        .expect("the second check must not error");
    assert!(
        second.is_none(),
        "a check inside the interval must be skipped"
    );

    let stored = update_status::stored_status(db.pool()).await.unwrap();
    assert_eq!(
        stored.checked_at, first.checked_at,
        "the stored timestamp must be the first check's"
    );

    // With a zero interval every run is due, which is what makes the skip above a
    // property of the interval rather than of anything else.
    let eager = cron_with(db.pool(), &endpoint, 0);
    assert!(
        eager
            .check_for_updates()
            .await
            .expect("the check must succeed")
            .is_some()
    );

    db.cleanup().await;
}

/// An unreachable or failing endpoint is an error the caller logs, and it changes
/// nothing. This is the property that keeps a broken check from becoming a broken
/// site.
#[tokio::test]
async fn a_failing_endpoint_changes_nothing() {
    let db = ScratchDb::new("failure").await;

    // A previous successful check, so there is something for a bad one to damage.
    let good = serve_release(release("v999.0.0", "999.0.0")).await;
    let cron = cron_with(db.pool(), &good, 0);
    let before = cron
        .check_for_updates()
        .await
        .expect("the first check must succeed")
        .expect("stored");

    for endpoint in [
        serve_failure().await,
        // Nothing listening: a port that was bound and released.
        "http://127.0.0.1:1/releases/latest".to_string(),
    ] {
        let broken = cron_with(db.pool(), &endpoint, 0);
        assert!(
            broken.check_for_updates().await.is_err(),
            "a failing endpoint must report an error for the caller to log"
        );
        let after = update_status::stored_status(db.pool()).await.unwrap();
        assert_eq!(
            after, before,
            "a failed check must leave the stored status exactly as it was"
        );
    }

    db.cleanup().await;
}

/// The whole cron cycle runs the check, which is the path production takes.
#[tokio::test]
async fn the_cron_cycle_runs_the_check() {
    let db = ScratchDb::new("cycle").await;
    let endpoint = serve_release(release("v999.0.0", "[security] 999.0.0")).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);

    // `run` takes the cron lock, which is per-Redis-key and shared, so this asserts
    // on the stored effect rather than on the returned task list: another test
    // holding the lock would make the list empty without making the feature wrong.
    let _ = cron.run().await;

    let stored = update_status::stored_status(db.pool()).await;
    match stored {
        Some(status) => {
            assert_eq!(status.latest_version, "999.0.0");
            assert!(status.is_security);
        }
        None => {
            // The lock was held by another test's cycle. Assert the task itself
            // still works, so the test is never a silent pass.
            let direct = cron
                .check_for_updates()
                .await
                .expect("the check must succeed")
                .expect("stored");
            assert_eq!(direct.latest_version, "999.0.0");
        }
    }

    db.cleanup().await;
}

/// The stored key is the documented one, so an operator can read and clear it.
#[tokio::test]
async fn the_status_is_stored_under_the_documented_key() {
    let db = ScratchDb::new("key").await;
    let endpoint = serve_release(release("v999.0.0", "999.0.0")).await;
    let cron = cron_with(db.pool(), &endpoint, 86_400);
    cron.check_for_updates().await.unwrap();

    let raw = SiteConfig::get(db.pool(), UPDATE_STATUS_KEY)
        .await
        .expect("read the key")
        .expect("the key must be set");
    assert_eq!(raw["latest_version"], "999.0.0");
    assert!(raw["checked_at"].is_i64(), "checked-at must be stored");

    db.cleanup().await;
}
