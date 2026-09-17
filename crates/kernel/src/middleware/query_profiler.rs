//! Request timing middleware.
//!
//! Applied to the whole router in `main.rs`, on every request: it adds a
//! `Server-Timing` response header and logs a request that takes too long.
//!
//! Configuration:
//! - `QUERY_SLOW_THRESHOLD_MS` (default: 100) — a request taking longer than
//!   this is logged at WARN, and longer than five times this at ERROR.
//!
//! The module used to claim a `query-profiler` cargo feature, which
//! `Cargo.toml` has never defined, and per-query profiling, which it has never
//! done: it measures the whole request. The name is the one thing left of that
//! ambition, kept because the setting it reads is named after it.

use std::time::Instant;

use axum::{body::Body, extract::State, http::Request, middleware::Next, response::Response};

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

/// Middleware that reports total request duration in a `Server-Timing` header.
///
/// Per-query tracking would require wrapping the `PgPool` and is not done here:
/// the measurement is the whole request, from the outermost layer inward, which
/// is what a browser's DevTools shows against `total;dur=`.
///
/// The threshold comes from the application state, resolved once at startup;
/// it used to be read from the environment on every single request.
pub async fn track_request_timing(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let start = Instant::now();
    let mut response = next.run(request).await;
    let elapsed_ms = start.elapsed().as_millis();

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
