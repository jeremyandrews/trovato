#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The site log records logins, and never a password (B107).
//!
//! Its own binary: the test installs the `system_log` layer as the global
//! `tracing` subscriber, as `main.rs` does, which a process can do once, and it
//! prunes the table by row count, which would touch other tests' rows.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app, test_ip_for};
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use trovato_kernel::system_log::{self, LogRecord, SystemLogLayer};
use uuid::Uuid;

/// Every stored row that mentions `needle` anywhere, message or context.
async fn rows_mentioning(db: &sqlx::PgPool, needle: &str) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT log_type, message, context::text FROM system_log \
         WHERE message LIKE $1 OR context::text LIKE $1 ORDER BY id",
    )
    .bind(format!("%{needle}%"))
    .fetch_all(db)
    .await
    .unwrap()
}

#[test]
fn a_failed_login_logs_the_username_and_never_the_password_and_retention_caps_rows() {
    run_test(async {
        let app = shared_app().await;
        let (layer, receiver) = SystemLogLayer::new();
        tracing_subscriber::registry()
            .with(layer.with_filter(LevelFilter::INFO))
            .init();
        system_log::spawn_writer(app.db.clone(), receiver);

        let name = common::username("b107logn");
        let password = "correct-horse-battery-staple";
        app.create_test_user(&name, password, &format!("{name}@test.local"))
            .await;
        app.state.lockout().clear_all(&name).await.ok();
        let wrong = format!("wrong-{}", Uuid::now_v7().simple());

        let ip = test_ip_for(&name);
        app.state.rate_limiter().reset("login", &ip).await.ok();
        let response = app
            .request(
                Request::post("/user/login/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "username": name, "password": wrong }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "the wrong password fails"
        );

        // And a good one, which is logged too.
        app.login(&name, password).await;

        // The writer is asynchronous.
        let mut rows = Vec::new();
        for _ in 0..50 {
            rows = rows_mentioning(&app.db, &name).await;
            if rows.len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            rows.iter()
                .any(|(t, m, c)| t == "user" && m.starts_with("login failed") && c.contains(&name)),
            "the failure is logged with the username: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|(t, m, _)| t == "user" && m == "user logged in"),
            "the success is logged: {rows:?}"
        );

        // **The finding.** No row anywhere holds either password.
        assert!(rows_mentioning(&app.db, &wrong).await.is_empty());
        assert!(rows_mentioning(&app.db, password).await.is_empty());

        // Retention by row count keeps the newest rows.
        let tag = Uuid::now_v7().simple().to_string();
        let now = chrono::Utc::now().timestamp();
        let records: Vec<LogRecord> = (0..5)
            .map(|i| LogRecord {
                created: now,
                level: "warn",
                log_type: "b107cap".to_string(),
                message: format!("row {i} {tag}"),
                context: Default::default(),
            })
            .collect();
        for record in records {
            system_log::insert(&app.db, &[record]).await.unwrap();
        }
        system_log::prune(&app.db, 30, 3).await.unwrap();
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM system_log")
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(total <= 3, "at most the cap is left, got {total}");
        let left: Vec<String> =
            sqlx::query_scalar("SELECT message FROM system_log WHERE message LIKE $1 ORDER BY id")
                .bind(format!("%{tag}"))
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert_eq!(
            left,
            (2..5).map(|i| format!("row {i} {tag}")).collect::<Vec<_>>(),
            "the newest rows are kept"
        );
    });
}
