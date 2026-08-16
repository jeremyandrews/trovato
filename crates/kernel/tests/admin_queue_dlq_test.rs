//! Integration tests for the plugin-queue dead-letter admin surface (P11d).
//!
//! Exercises the real HTTP router: admin auth, CSRF on mutations, and the
//! list/requeue/delete endpoints over `plugin_queue` rows in `status = 'dead'`.
//! Uses a dedicated `plugin_name` (no worker) so it never interferes with the
//! drain or the queue-v2 tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};
use sqlx::Row;

const DLQ_PLUGIN: &str = "test_dlq_admin";

/// Extract the CSRF token from the admin page for header-based CSRF.
async fn csrf_token(app: &common::TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get("/admin").body(Body::empty()).unwrap(), cookies)
        .await;
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    if let Some(pos) = html.find("csrf-token")
        && let Some(cs) = html[pos..].find("content=\"").map(|p| pos + p + 9)
    {
        let end = html[cs..].find('"').map(|p| cs + p).unwrap_or(cs);
        return html[cs..end].to_string();
    }
    String::new()
}

/// Insert a dead-lettered row directly; returns its id.
async fn insert_dead(app: &common::TestApp, reason: &str) -> i64 {
    let now = chrono::Utc::now().timestamp();
    let row = sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, status, attempts,
             max_attempts, dead_reason, dead_at, last_error)
        VALUES ($1, 'q', '{"x":1}'::jsonb, $2, 'dead', 3, 3, $3, $2, $3)
        RETURNING id
        "#,
    )
    .bind(DLQ_PLUGIN)
    .bind(now)
    .bind(reason)
    .fetch_one(&app.db)
    .await
    .unwrap();
    row.get::<i64, _>("id")
}

async fn status_of(app: &common::TestApp, id: i64) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT status FROM plugin_queue WHERE id = $1")
        .bind(id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
}

#[test]
fn dlq_list_requires_admin() {
    run_test(async {
        let app = shared_app().await;
        let response = app
            .request(
                Request::get("/admin/queue/dlq")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        // Not authenticated → not a 200.
        assert_ne!(response.status(), StatusCode::OK);
    });
}

#[test]
fn dlq_list_shows_dead_jobs_for_admin() {
    run_test(async {
        let app = shared_app().await;
        let id = insert_dead(app, "dlq_list_reason").await;

        let cookies = app
            .create_and_login_admin("dlq_list_admin", "TestPassword123!", "dlq_list@test.com")
            .await;

        let response = app
            .request_with_cookies(
                Request::get("/admin/queue/dlq")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 5_000_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let jobs = json["jobs"].as_array().unwrap();
        let found = jobs.iter().any(|j| j["id"].as_i64() == Some(id));
        assert!(found, "inserted dead job should be listed");

        sqlx::query("DELETE FROM plugin_queue WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

#[test]
fn dlq_requeue_resets_job_to_ready() {
    run_test(async {
        let app = shared_app().await;
        let id = insert_dead(app, "requeue_reason").await;

        let cookies = app
            .create_and_login_admin("dlq_requeue_admin", "TestPassword123!", "dlq_rq@test.com")
            .await;
        let csrf = csrf_token(app, &cookies).await;

        // Without CSRF → forbidden.
        let no_csrf = app
            .request_with_cookies(
                Request::post(format!("/admin/queue/dlq/{id}/requeue"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);
        assert_eq!(status_of(app, id).await.as_deref(), Some("dead"));

        // With CSRF → requeued.
        let ok = app
            .request_with_cookies(
                Request::post(format!("/admin/queue/dlq/{id}/requeue"))
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(status_of(app, id).await.as_deref(), Some("ready"));

        // Attempts were cleared and dead fields nulled.
        let (attempts, dead_reason): (i32, Option<String>) =
            sqlx::query_as("SELECT attempts, dead_reason FROM plugin_queue WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(attempts, 0);
        assert!(dead_reason.is_none());

        sqlx::query("DELETE FROM plugin_queue WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

#[test]
fn dlq_delete_removes_job() {
    run_test(async {
        let app = shared_app().await;
        let id = insert_dead(app, "delete_reason").await;

        let cookies = app
            .create_and_login_admin("dlq_delete_admin", "TestPassword123!", "dlq_del@test.com")
            .await;
        let csrf = csrf_token(app, &cookies).await;

        let ok = app
            .request_with_cookies(
                Request::post(format!("/admin/queue/dlq/{id}/delete"))
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert!(status_of(app, id).await.is_none(), "dead job deleted");
    });
}
