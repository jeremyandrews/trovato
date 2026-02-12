//! Integration tests for the Trovato kernel.
//!
//! ## Prerequisites
//!
//! These tests require Postgres and Redis running via docker-compose:
//!
//! ```bash
//! docker-compose up -d
//! ```
//!
//! ## Running Tests
//!
//! ```bash
//! cargo test --test integration_test
//! ```
//!
//! ## Test Isolation
//!
//! Each test creates a fresh `TestApp` instance that:
//! - Runs database migrations
//! - Clears test data from previous runs (users matching `test%` or `lock%`)
//! - Clears Redis lockout keys for test users
//! - Clears password reset tokens
//!
//! ## Adding New Tests
//!
//! 1. Add new route handlers to `tests/common/mod.rs` following existing patterns
//! 2. Add test functions here grouped by feature (use `// ===` section headers)
//! 3. Use `TestApp::new().await` for setup
//! 4. Use `app.request(...)` to send HTTP requests
//! 5. Use `response_json(response).await` to parse JSON responses
//! 6. If your test uses new usernames, add cleanup in `TestApp::cleanup_test_data`
//!
//! ## Current Coverage
//!
//! - Health check (`/health`)
//! - Authentication (`/user/login`, `/user/logout`)
//! - Account lockout (5 failed attempts triggers 15-minute lockout)
//! - Password reset (`/user/password-reset`, `/user/password-reset/{token}`)
//! - Stage switching (`/admin/stage/switch`, `/admin/stage/current`)

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};

mod common;
use common::TestApp;

// =============================================================================
// Health Check Tests
// =============================================================================

#[tokio::test]
async fn health_check_returns_healthy() {
    let app = TestApp::new().await;

    let response = app
        .request(Request::get("/health").body(Body::empty()).unwrap())
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["status"], "healthy");
    assert_eq!(body["postgres"], true);
    assert_eq!(body["redis"], true);
}

// =============================================================================
// Authentication Tests
// =============================================================================

#[tokio::test]
async fn login_with_invalid_credentials_returns_401() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::post("/user/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "username": "nonexistent",
                        "password": "wrongpassword"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let body = response_json(response).await;
    assert_eq!(body["error"], "Invalid username or password");
}

#[tokio::test]
async fn login_with_valid_credentials_returns_success() {
    let app = TestApp::new().await;

    // Create a test user first
    app.create_test_user("testuser", "testpass123", "test@example.com")
        .await;

    let response = app
        .request(
            Request::post("/user/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "username": "testuser",
                        "password": "testpass123"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["message"], "Login successful");
}

#[tokio::test]
async fn logout_clears_session() {
    let app = TestApp::new().await;

    let response = app
        .request(Request::get("/user/logout").body(Body::empty()).unwrap())
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["success"], true);
}

// =============================================================================
// Account Lockout Tests
// =============================================================================

#[tokio::test]
async fn account_locks_after_failed_attempts() {
    let app = TestApp::new().await;

    // Create a test user
    app.create_test_user("locktest", "correctpass", "lock@example.com")
        .await;

    // Make 5 failed login attempts
    for i in 0..5 {
        let response = app
            .request(
                Request::post("/user/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "username": "locktest",
                            "password": "wrongpassword"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;

        if i < 4 {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        } else {
            // 5th attempt triggers lockout
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        }
    }

    // Now even correct password should fail
    let response = app
        .request(
            Request::post("/user/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "username": "locktest",
                        "password": "correctpass"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

// =============================================================================
// Password Reset Tests
// =============================================================================

#[tokio::test]
async fn password_reset_request_always_succeeds() {
    let app = TestApp::new().await;

    // Request reset for non-existent email (should still return success for security)
    let response = app
        .request(
            Request::post("/user/password-reset")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "email": "nonexistent@example.com"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn password_reset_with_invalid_token_fails() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/user/password-reset/invalidtoken123")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_json(response).await;
    assert_eq!(body["error"], "Invalid or expired reset token");
}

// =============================================================================
// Stage Switching Tests
// =============================================================================

#[tokio::test]
async fn stage_switch_updates_session() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::post("/admin/stage/switch")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "stage_id": "preview"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["active_stage"], "preview");
}

#[tokio::test]
async fn stage_current_returns_active_stage() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/admin/stage/current")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["success"], true);
    // Default stage is None (live)
    assert_eq!(body["active_stage"], Value::Null);
}

// =============================================================================
// Helpers
// =============================================================================

async fn response_json(response: axum::response::Response) -> Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}
