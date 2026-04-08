//! Integration tests for the AI assist API endpoint.
//!
//! Tests auth, permissions, validation, and error handling paths.
//! These do NOT require a real AI provider — they test the kernel's
//! request handling before provider resolution.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};

/// Helper: extract CSRF token from a session by making a GET to a form page.
async fn get_csrf_token(app: &common::TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get("/admin").body(Body::empty()).unwrap(), cookies)
        .await;

    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);

    // Extract CSRF token from meta tag or hidden input
    if let Some(pos) = html.find("name=\"_token\"") {
        let value_start = html[pos..].find("value=\"").map(|p| pos + p + 7);
        if let Some(start) = value_start {
            let end = html[start..].find('"').map(|p| start + p).unwrap_or(start);
            return html[start..end].to_string();
        }
    }

    // Fallback: try meta tag
    if let Some(pos) = html.find("csrf-token") {
        let content_start = html[pos..].find("content=\"").map(|p| pos + p + 9);
        if let Some(start) = content_start {
            let end = html[start..].find('"').map(|p| start + p).unwrap_or(start);
            return html[start..end].to_string();
        }
    }

    String::new()
}

#[test]
fn ai_assist_requires_authentication() {
    run_test(async {
        let app = shared_app().await;

        let response = app
            .request(
                Request::post("/api/v1/ai/assist")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "text": "Hello world",
                            "operation": "rewrite"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;

        // CSRF check fires before auth check, so unauthenticated requests
        // without a CSRF token get 403 (Forbidden) not 401 (Unauthorized).
        assert!(
            response.status() == StatusCode::FORBIDDEN
                || response.status() == StatusCode::UNAUTHORIZED,
            "expected 401 or 403, got {}",
            response.status()
        );
    });
}

#[test]
fn ai_assist_validates_empty_text() {
    run_test(async {
        let app = shared_app().await;

        let cookies = app
            .create_and_login_admin("ai_assist_empty", "TestPassword123!", "ai_empty@test.com")
            .await;

        let csrf = get_csrf_token(app, &cookies).await;

        let response = app
            .request_with_cookies(
                Request::post("/api/v1/ai/assist")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({
                            "text": "",
                            "operation": "rewrite"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["message"].as_str().unwrap().contains("empty"));
    });
}

#[test]
fn ai_assist_validates_invalid_operation() {
    run_test(async {
        let app = shared_app().await;

        let cookies = app
            .create_and_login_admin("ai_assist_badop", "TestPassword123!", "ai_badop@test.com")
            .await;

        let csrf = get_csrf_token(app, &cookies).await;

        let response = app
            .request_with_cookies(
                Request::post("/api/v1/ai/assist")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({
                            "text": "Hello world",
                            "operation": "invalid_op"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["message"]
                .as_str()
                .unwrap()
                .contains("Invalid operation")
        );
    });
}

#[test]
fn ai_assist_validates_text_too_long() {
    run_test(async {
        let app = shared_app().await;

        let cookies = app
            .create_and_login_admin("ai_assist_long", "TestPassword123!", "ai_long@test.com")
            .await;

        let csrf = get_csrf_token(app, &cookies).await;

        // Text over 10,000 characters
        let long_text = "x".repeat(10_001);

        let response = app
            .request_with_cookies(
                Request::post("/api/v1/ai/assist")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({
                            "text": long_text,
                            "operation": "shorten"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["message"].as_str().unwrap().contains("too long"));
    });
}

#[test]
fn ai_assist_returns_503_without_provider() {
    run_test(async {
        let app = shared_app().await;

        let cookies = app
            .create_and_login_admin("ai_assist_noprov", "TestPassword123!", "ai_noprov@test.com")
            .await;

        let csrf = get_csrf_token(app, &cookies).await;

        // Valid request but no AI provider configured in test environment
        let response = app
            .request_with_cookies(
                Request::post("/api/v1/ai/assist")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({
                            "text": "Hello world",
                            "operation": "rewrite"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;

        // Should be 503 Service Unavailable (no provider configured)
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    });
}
