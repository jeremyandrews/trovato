//! Request timing middleware.
//!
//! Applied to the whole router in `main.rs`, on every request: it adds a
//! `Server-Timing` response header and logs a request that takes too long.
//!
//! Configuration:
//! - `QUERY_SLOW_THRESHOLD_MS` (default: 100) — a request taking longer than
//!   this is logged at WARN, and longer than five times this at ERROR.
//!
//! It is also where a request becomes something the request profiler can see:
//! it opens the profiler's root span for the request (method, matched route,
//! status; nothing else), and counts the request into `http_requests_total`.
//! Per-query, per-tap and per-template timing are not measured here: they come
//! from `tracing` spans at those points and sqlx's own statement events,
//! collected by [`crate::profiler`] when `PROFILER_ENABLED` is set. With it off
//! the span is disabled at its callsite and this middleware does what it did
//! before the profiler existed. The module keeps its name because the setting
//! it reads, `QUERY_SLOW_THRESHOLD_MS`, is named after it.

use std::time::Instant;

use tracing::Instrument;

use axum::{
    body::Body,
    extract::{MatchedPath, State},
    http::Request,
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

/// How a request's duration compares to the configured slow threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Speed {
    /// At or under the threshold.
    Normal,
    /// Over the threshold.
    Slow,
    /// Over five times the threshold.
    VerySlow,
}

/// Classify a request duration against the slow threshold.
///
/// A parameterized core rather than an inline comparison so the boundaries are
/// testable: "slow" is strictly *over* the threshold, and "very slow" strictly
/// over five times it, so a threshold of 100 leaves exactly 100 ms normal.
pub(crate) fn classify(elapsed_ms: u128, threshold_ms: u128) -> Speed {
    if elapsed_ms > threshold_ms.saturating_mul(5) {
        Speed::VerySlow
    } else if elapsed_ms > threshold_ms {
        Speed::Slow
    } else {
        Speed::Normal
    }
}

/// The route label of a request no route matched, which the router's fallback
/// answers: a URL alias it resolves and serves, or a 404. One series for all of
/// them, rather than one per path asked for.
const UNMATCHED_ROUTE: &str = "<fallback>";

/// Middleware that reports total request duration in a `Server-Timing` header.
///
/// It also opens the request profiler's root span, and counts the request into `http_requests_total` and
/// `http_request_duration_seconds`, labelled by method, status and the
/// matched route template (`/item/{id}`, never the raw path), so the label
/// set stays as small as the route table.
///
/// The `Server-Timing` measurement is the whole request, from the outermost
/// layer inward, which is what a browser's DevTools shows against `total;dur=`.
/// Routing happens before this layer runs (axum matches the route first), so
/// it is not part of any measurement here; the profile records which route
/// matched.
///
/// The threshold comes from the application state, resolved once at startup;
/// it used to be read from the environment on every single request.
pub async fn track_request_timing(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let start = Instant::now();
    let method = request.method().clone();
    // The router has matched by the time a `Router::layer` middleware runs, so
    // the template is already in the extensions.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    let route = route.as_deref().unwrap_or(UNMATCHED_ROUTE);
    let span = crate::profiler::request_span(method.as_str(), route);
    let mut response = next.run(request).instrument(span.clone()).await;
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();
    crate::profiler::record_status(&span, response.status().as_u16());

    state.metrics().record_request(
        method.as_str(),
        route,
        response.status().as_u16(),
        elapsed.as_secs_f64(),
    );

    // Add Server-Timing header for browser DevTools
    let timing_value = format!("total;dur={elapsed_ms}");
    if let Ok(val) = timing_value.parse() {
        response.headers_mut().insert("server-timing", val);
    }

    match classify(elapsed_ms, state.runtime().slow_request_threshold_ms) {
        Speed::VerySlow => {
            tracing::error!(elapsed_ms = elapsed_ms, "very slow request (>5x threshold)");
        }
        Speed::Slow => tracing::warn!(elapsed_ms = elapsed_ms, "slow request"),
        Speed::Normal => {}
    }

    response
}

/// Middleware that marks the handler as a phase of the request profile.
///
/// Installed innermost, around the routes, and only when the profiler is on:
/// off, it is not in the stack at all. What lies between the request span and
/// this one is the rest of the middleware (sessions, authentication, rate
/// limits).
pub async fn profile_handler(request: Request<Body>, next: Next) -> Response {
    next.run(request)
        .instrument(crate::profiler::phase("handler", ""))
        .await
}

#[cfg(test)]
mod tests {
    use super::{Speed, classify};

    /// The boundaries are exclusive on both steps, which is what keeps a request
    /// exactly at the threshold out of the log.
    #[test]
    fn classification_boundaries_are_exclusive() {
        assert_eq!(classify(0, 100), Speed::Normal);
        assert_eq!(classify(100, 100), Speed::Normal);
        assert_eq!(classify(101, 100), Speed::Slow);
        assert_eq!(classify(500, 100), Speed::Slow);
        assert_eq!(classify(501, 100), Speed::VerySlow);
    }

    /// A zero threshold logs everything except an instantaneous request, rather
    /// than dividing by zero or wrapping.
    #[test]
    fn a_zero_threshold_does_not_panic() {
        assert_eq!(classify(0, 0), Speed::Normal);
        assert_eq!(classify(1, 0), Speed::VerySlow);
    }

    /// `threshold * 5` overflowing `u128` would panic in debug builds; the
    /// saturating multiply means an absurd threshold just never fires.
    #[test]
    fn an_absurd_threshold_saturates_instead_of_overflowing() {
        assert_eq!(classify(u128::MAX, u128::MAX), Speed::Normal);
        assert_eq!(classify(u128::MAX - 1, u128::MAX), Speed::Normal);
    }
}
