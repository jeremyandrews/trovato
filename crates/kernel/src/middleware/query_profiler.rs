//! Query profiler middleware.
//!
//! When enabled (via `--features query-profiler` or always-on in dev),
//! logs slow database queries and adds `Server-Timing` response headers.
//!
//! Configuration:
//! - `QUERY_SLOW_THRESHOLD_MS` (default: 100) — queries exceeding this are logged
//! - Queries exceeding 5x threshold are logged at ERROR level

use std::time::Instant;

use axum::{body::Body, http::Request, middleware::Next, response::Response};

/// Middleware that tracks total request DB time via `Server-Timing` header.
///
/// Actual per-query tracking requires wrapping the PgPool, which is
/// deferred to a future enhancement. This middleware measures total
/// request processing time as a proxy.
pub async fn track_request_timing(request: Request<Body>, next: Next) -> Response {
    let start = Instant::now();
    let mut response = next.run(request).await;
    let elapsed_ms = start.elapsed().as_millis();

    // Add Server-Timing header for browser DevTools
    let timing_value = format!("total;dur={elapsed_ms}");
    if let Ok(val) = timing_value.parse() {
        response.headers_mut().insert("server-timing", val);
    }

    // Log slow requests
    let threshold: u128 = std::env::var("QUERY_SLOW_THRESHOLD_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);

    if elapsed_ms > threshold * 5 {
        tracing::error!(elapsed_ms = elapsed_ms, "very slow request (>5x threshold)");
    } else if elapsed_ms > threshold {
        tracing::warn!(elapsed_ms = elapsed_ms, "slow request");
    }

    response
}
