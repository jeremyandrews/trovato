#![allow(clippy::unwrap_used, clippy::expect_used)]
//! One JSON error shape, on every router.
//!
//! Every JSON error the kernel writes is an `AppError` body:
//! `{"code", "message", "request_id"}` (plus `details` for a validation
//! failure). Before this, a JSON caller could get any of five shapes depending
//! on the route: `AppError`, `{"error"}` from `JsonError` and five local copies
//! of it, `{"error","status"}` from `/api/v1`, `{"error","retry_after"}` from the
//! rate limiter, and plain text from the content-lock routes and plugin gates.
//!
//! One row per router (or middleware), each a request that router refuses or
//! fails, with the status that route has always answered.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};

struct Row {
    router: &'static str,
    method: &'static str,
    path: &'static str,
    content_type: Option<&'static str>,
    authorization: Option<&'static str>,
    body: &'static str,
    status: StatusCode,
}

const fn row(
    router: &'static str,
    method: &'static str,
    path: &'static str,
    status: StatusCode,
) -> Row {
    Row {
        router,
        method,
        path,
        content_type: None,
        authorization: None,
        body: "",
        status,
    }
}

const fn json(
    router: &'static str,
    method: &'static str,
    path: &'static str,
    body: &'static str,
    status: StatusCode,
) -> Row {
    Row {
        router,
        method,
        path,
        content_type: Some("application/json"),
        authorization: None,
        body,
        status,
    }
}

const MISSING: &str = "00000000-0000-0000-0000-00000000b840";

const ROWS: &[Row] = &[
    row("api_token", "GET", "/api/tokens", StatusCode::UNAUTHORIZED),
    row(
        "api_v1",
        "GET",
        "/api/v1/user/export",
        StatusCode::UNAUTHORIZED,
    ),
    row(
        "admin_queue",
        "GET",
        "/admin/queue/dlq",
        StatusCode::UNAUTHORIZED,
    ),
    row(
        "batch",
        "GET",
        "/api/batch/00000000-0000-0000-0000-00000000b840",
        StatusCode::UNAUTHORIZED,
    ),
    json(
        "category",
        "POST",
        "/api/category",
        r#"{"id":"b84","label":"B84","hierarchy":0}"#,
        StatusCode::UNAUTHORIZED,
    ),
    row(
        "category (not found)",
        "GET",
        "/api/category/b84-no-such-category",
        StatusCode::NOT_FOUND,
    ),
    row(
        "comment",
        "GET",
        "/api/comment/00000000-0000-0000-0000-00000000b840",
        StatusCode::NOT_FOUND,
    ),
    row(
        "gather",
        "GET",
        "/api/query/b84_no_such_query",
        StatusCode::NOT_FOUND,
    ),
    json(
        "lock",
        "POST",
        "/api/lock/heartbeat",
        r#"{"entity_type":"nonsense","entity_id":"x"}"#,
        StatusCode::BAD_REQUEST,
    ),
    Row {
        router: "user_delete",
        method: "POST",
        path: "/user/delete/passkey/start",
        content_type: None,
        authorization: None,
        body: "",
        status: StatusCode::UNAUTHORIZED,
    },
    json(
        "webauthn",
        "POST",
        "/user/passkeys/00000000-0000-0000-0000-00000000b840/rename",
        r#"{"name":"x"}"#,
        StatusCode::UNAUTHORIZED,
    ),
    json(
        "sessions",
        "POST",
        "/user/sessions/00000000-0000-0000-0000-00000000b840/rename",
        r#"{"name":"x"}"#,
        StatusCode::UNAUTHORIZED,
    ),
    json(
        "api_chat",
        "POST",
        "/api/v1/chat",
        r#"{"message":"x"}"#,
        StatusCode::FORBIDDEN,
    ),
    json(
        "item",
        "POST",
        "/item/add/page",
        r#"{"title":"x"}"#,
        StatusCode::UNAUTHORIZED,
    ),
    Row {
        router: "api_token middleware",
        method: "GET",
        path: "/api/tokens",
        content_type: None,
        authorization: Some("Bearer trovato_b84_not_a_token"),
        body: "",
        status: StatusCode::UNAUTHORIZED,
    },
];

async fn send(app: &TestApp, row: &Row) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(row.method)
        .uri(row.path.replace("{missing}", MISSING))
        .header("x-forwarded-for", test_ip_for("b84-shape"));
    if let Some(content_type) = row.content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    if let Some(authorization) = row.authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    let response = app
        .request(builder.body(Body::from(row.body)).unwrap())
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The canonical body: `code`, `message` and `request_id` as strings, and no
/// key from the shapes it replaced.
fn assert_app_error(body: &str, what: &str) {
    let value: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|_| panic!("{what}: body is not JSON: {body}"));
    for key in ["code", "message", "request_id"] {
        assert!(
            value.get(key).and_then(|v| v.as_str()).is_some(),
            "{what}: AppError body has no `{key}`: {body}"
        );
    }
    for stale in ["error", "status", "retry_after"] {
        assert!(
            value.get(stale).is_none(),
            "{what}: body still carries `{stale}`: {body}"
        );
    }
}

#[test]
fn every_router_answers_errors_as_app_error() {
    run_test(async {
        let app = shared_app().await;
        for plugin in [
            "trovato_categories",
            "trovato_comments",
            "trovato_content_locking",
        ] {
            app.ensure_plugin_enabled(plugin).await;
        }
        for row in ROWS {
            let what = format!("{} ({} {})", row.router, row.method, row.path);
            let (status, body) = send(app, row).await;
            assert_eq!(status, row.status, "{what}: {body}");
            assert_app_error(&body, &what);
        }
    });
}

/// The rate limiter's 429 is an `AppError` too, with `Retry-After` kept.
#[test]
fn the_rate_limit_answer_is_app_error() {
    let response = trovato_kernel::middleware::rate_limit_response(42);
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("42")
    );
    run_test(async move {
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = String::from_utf8_lossy(&bytes).into_owned();
        assert_app_error(&body, "rate limit");
        assert!(body.contains("rate_limited"), "{body}");
    });
}
