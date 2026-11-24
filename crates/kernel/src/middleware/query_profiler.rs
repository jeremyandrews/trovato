//! Request timing middleware: Gander's outermost layer.
//!
//! Applied to the whole router in `main.rs`, on every request. It runs the
//! request inside a [`RequestProfile`], so every query, cache call, plugin
//! call, render and outgoing request the request makes on its own task is
//! counted; it opens the `http.request` span the request's other spans hang
//! from; and afterwards it:
//!
//! - adds a `Server-Timing` header: `total;dur=<ms>` for everyone, and the
//!   counts and durations as well for whoever `SERVER_TIMING` allows (an
//!   administrator, by default);
//! - counts the request into the HTTP metrics by route template;
//! - logs a request that took too long, with its breakdown;
//! - leaves a [`RequestProfileSnapshot`] in the response's extensions, which is
//!   how the performance tests read a request's numbers.
//!
//! Configuration:
//! - `QUERY_SLOW_THRESHOLD_MS` (default: 100) — a request taking longer than
//!   this is logged at WARN, and longer than five times this at ERROR.
//! - `SERVER_TIMING` (default: `admin`) — who sees the detailed header.
//!
//! See `docs/design/tracing-and-performance-tests.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{MatchedPath, State},
    http::Request,
    middleware::Next,
    response::Response,
};
use tracing::Instrument;
use tracing::field::Empty;

use crate::state::AppState;
use crate::telemetry::{self, RequestProfile, RequestProfileSnapshot, method_label};

/// Who sees the detailed `Server-Timing` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerTimingDetail {
    /// Administrators only (the default): counts and durations per request
    /// are a timing oracle in anyone else's hands.
    #[default]
    Admin,
    /// Every visitor: for a development or benchmark site.
    All,
    /// No one: `total` only, as before.
    Off,
}

impl ServerTimingDetail {
    /// Read `SERVER_TIMING`. Anything unrecognised is the default, so a typo
    /// never shows the detail to everyone.
    pub fn from_setting(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("all") => Self::All,
            Some("off" | "none") => Self::Off,
            _ => Self::Admin,
        }
    }
}

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
pub const UNMATCHED_ROUTE: &str = "<fallback>";

/// Milliseconds with one decimal, the precision `Server-Timing` readers show.
fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

/// The `Server-Timing` value: `total` alone, or with the request's breakdown.
///
/// Every value in it is a number the kernel produced: no path, no name, no
/// statement.
pub(crate) fn server_timing(total: Duration, detail: Option<&RequestProfileSnapshot>) -> String {
    let mut value = format!("total;dur={}", total.as_millis());
    if let Some(s) = detail {
        value.push_str(&format!(
            ", db;dur={};desc=\"{} queries\"\
             , cache;desc=\"{} gets {} misses {} sets\"\
             , plugin;dur={};desc=\"{} calls {} host calls\"\
             , render;dur={};desc=\"{} renders\"\
             , http;desc=\"{} calls\"",
            ms(s.query_time),
            s.queries,
            s.cache_gets,
            s.cache_misses,
            s.cache_sets,
            ms(s.plugin_time),
            s.plugin_calls,
            s.host_calls,
            ms(s.render_time),
            s.renders,
            s.outgoing_calls,
        ));
    }
    value
}

/// Whether this request's viewer sees the detailed header.
///
/// Decided after the profile is read, so the user lookup an administrator
/// check makes is never part of the request's own counts.
async fn shows_detail(state: &AppState, profile: &RequestProfile) -> bool {
    match state.runtime().server_timing {
        ServerTimingDetail::All => true,
        ServerTimingDetail::Off => false,
        ServerTimingDetail::Admin => match profile.user_id() {
            Some(id) => matches!(
                state.users().find_by_id(id).await,
                Ok(Some(user)) if user.is_admin && user.is_active()
            ),
            None => false,
        },
    }
}

/// Middleware that profiles the request and reports what it did.
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
    let route_label = route.as_deref().unwrap_or(UNMATCHED_ROUTE);

    let profile = Arc::new(RequestProfile::new(
        Some(state.metrics().clone()),
        state.runtime().profile_fingerprints,
    ));
    // Only the route template, never the path: a path carries ids, terms and
    // tokens.
    let span = tracing::trace_span!(
        target: telemetry::TARGET,
        "http.request",
        // A backend's name for the span (`GET /item/{id}`); field values are
        // only evaluated when the span is enabled.
        otel.name = %format_args!("{} {route_label}", method_label(&method)),
        otel.kind = "server",
        http.request.method = method_label(&method),
        http.route = route_label,
        http.response.status_code = Empty,
        trovato.db.queries = Empty,
        trovato.cache.gets = Empty,
        trovato.cache.misses = Empty,
        trovato.plugin.calls = Empty,
    );
    let mut response =
        telemetry::profile::scope(profile.clone(), next.run(request).instrument(span.clone()))
            .await;
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let snapshot = profile.snapshot();
    let status = response.status().as_u16();

    span.record("http.response.status_code", status);
    span.record("trovato.db.queries", snapshot.queries);
    span.record("trovato.cache.gets", snapshot.cache_gets);
    span.record("trovato.cache.misses", snapshot.cache_misses);
    span.record("trovato.plugin.calls", snapshot.plugin_calls);

    let metrics = state.metrics();
    metrics.record_request(
        method_label(&method),
        route_label,
        status,
        elapsed.as_secs_f64(),
    );
    metrics.record_request_queries(route_label, snapshot.queries);

    let detail = shows_detail(&state, &profile).await;
    let timing_value = server_timing(elapsed, detail.then_some(&snapshot));
    if let Ok(val) = timing_value.parse() {
        response.headers_mut().insert("server-timing", val);
    }

    let speed = classify(elapsed_ms, state.runtime().slow_request_threshold_ms);
    if speed != Speed::Normal {
        // The breakdown the Gander design asked for, and nothing that names
        // what was asked for: the route template, counts and durations.
        macro_rules! slow {
            ($level:ident, $msg:literal) => {
                tracing::$level!(
                    elapsed_ms = elapsed_ms,
                    route = route_label,
                    status = status,
                    queries = snapshot.queries,
                    db_ms = snapshot.query_time.as_millis(),
                    cache_gets = snapshot.cache_gets,
                    cache_misses = snapshot.cache_misses,
                    plugin_calls = snapshot.plugin_calls,
                    plugin_ms = snapshot.plugin_time.as_millis(),
                    render_ms = snapshot.render_time.as_millis(),
                    outgoing_calls = snapshot.outgoing_calls,
                    $msg
                )
            };
        }
        if speed == Speed::VerySlow {
            slow!(error, "very slow request (>5x threshold)");
        } else {
            slow!(warn, "slow request");
        }
    }

    response.extensions_mut().insert(snapshot);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn telemetry_names_only_standard_methods() {
        use axum::http::Method;
        for method in [
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::CONNECT,
            Method::OPTIONS,
            Method::TRACE,
            Method::PATCH,
        ] {
            assert_eq!(method_label(&method), method.as_str());
        }
        // Methods are case sensitive: `get` is not `GET`.
        for invented in ["ALICE-AT-EXAMPLE-COM", "get"] {
            let method = Method::from_bytes(invented.as_bytes()).unwrap();
            assert_eq!(method_label(&method), "_OTHER", "{invented}");
        }
    }

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

    /// Without detail the header is exactly what every visitor got before.
    #[test]
    fn the_plain_header_is_total_only() {
        assert_eq!(
            server_timing(Duration::from_millis(12), None),
            "total;dur=12"
        );
    }

    #[test]
    fn the_detailed_header_carries_the_counts() {
        let s = RequestProfileSnapshot {
            queries: 7,
            query_time: Duration::from_micros(2500),
            cache_gets: 4,
            cache_misses: 1,
            cache_sets: 1,
            plugin_calls: 2,
            host_calls: 3,
            renders: 1,
            ..RequestProfileSnapshot::default()
        };
        let v = server_timing(Duration::from_millis(12), Some(&s));
        assert!(
            v.starts_with("total;dur=12, db;dur=2.5;desc=\"7 queries\""),
            "{v}"
        );
        assert!(v.contains("cache;desc=\"4 gets 1 misses 1 sets\""), "{v}");
        assert!(
            v.contains("plugin;dur=0.0;desc=\"2 calls 3 host calls\""),
            "{v}"
        );
        // A valid header value.
        assert!(v.parse::<axum::http::HeaderValue>().is_ok());
    }

    #[test]
    fn an_unknown_setting_is_the_default() {
        assert_eq!(
            ServerTimingDetail::from_setting(None),
            ServerTimingDetail::Admin
        );
        assert_eq!(
            ServerTimingDetail::from_setting(Some("everyone")),
            ServerTimingDetail::Admin
        );
        assert_eq!(
            ServerTimingDetail::from_setting(Some(" ALL ")),
            ServerTimingDetail::All
        );
        assert_eq!(
            ServerTimingDetail::from_setting(Some("off")),
            ServerTimingDetail::Off
        );
    }
}
