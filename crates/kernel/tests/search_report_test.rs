#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The search report screen and the browser's search beacon.
//!
//! `/admin/reports/search` is for site administrators; `POST /api/search/log`
//! is how a browser search, which runs entirely in Pagefind, is counted. The
//! beacon validates what it is sent and answers 204 without touching the
//! database.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};

async fn beacon(app: &TestApp, body: serde_json::Value) -> StatusCode {
    app.request(
        Request::post("/api/search/log")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .status()
}

// =============================================================================
// (e) The report is for site administrators
// =============================================================================

#[test]
fn the_report_is_for_site_administrators_only() {
    run_test(async {
        let app = shared_app().await;

        let name = common::username("srchrpt");
        let admin = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let response = app
            .request_with_cookies(
                Request::get("/admin/reports/search?days=7")
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let (_, plain) = common::user_holding(app, "srchrptusr", &[]).await;
        for cookies in [plain.as_str(), ""] {
            let status = app
                .request_with_cookies(
                    Request::get("/admin/reports/search")
                        .body(Body::empty())
                        .unwrap(),
                    cookies,
                )
                .await
                .status();
            assert!(
                status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
                "the report must be refused without `administer site`, got {status}"
            );
        }
    });
}

// =============================================================================
// (f) The beacon validates what it is sent
// =============================================================================

#[test]
fn the_beacon_accepts_a_search_and_answers_no_content() {
    run_test(async {
        let app = shared_app().await;
        assert_eq!(
            beacon(
                app,
                serde_json::json!({"query": "rust conferences", "results": 4})
            )
            .await,
            StatusCode::NO_CONTENT
        );
    });
}

#[test]
fn the_beacon_rejects_an_oversized_query() {
    run_test(async {
        let app = shared_app().await;
        let query = "x".repeat(501);
        assert_eq!(
            beacon(app, serde_json::json!({"query": query, "results": 1})).await,
            StatusCode::BAD_REQUEST
        );
    });
}

#[test]
fn the_beacon_rejects_a_negative_or_absurd_result_count() {
    run_test(async {
        let app = shared_app().await;
        for results in [-1_i64, 100_001] {
            assert_eq!(
                beacon(
                    app,
                    serde_json::json!({"query": "rust", "results": results})
                )
                .await,
                StatusCode::BAD_REQUEST,
                "results = {results}"
            );
        }
    });
}
