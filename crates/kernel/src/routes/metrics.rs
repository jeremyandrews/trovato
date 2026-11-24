//! Prometheus metrics endpoint.

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::state::AppState;

/// Create the metrics router.
pub fn router() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics))
}

/// Prometheus metrics endpoint.
///
/// Returns metrics in Prometheus text exposition format, to an operator only
/// (`METRICS_TOKEN`, or a loopback peer that is not a proxy: see
/// [`crate::routes::health::is_operator`]). Anyone else gets a 404, as if
/// there were no such route: route templates, pool sizes and request rates
/// are nobody else's business.
async fn metrics(State(state): State<AppState>, request: Request) -> Response {
    if !crate::routes::health::is_operator(&request, state.runtime().metrics_token.as_deref()) {
        // Never stored: a cache in front must not hand an operator's answer
        // to the next caller, nor remember a refusal.
        let mut refused = crate::routes::helpers::render_not_found();
        refused.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        return refused;
    }
    // Update database pool gauges before encoding
    let pool = state.db();
    let pool_size = pool.size();
    let pool_idle = pool.num_idle() as u32;
    let pool_active = pool_size.saturating_sub(pool_idle);
    let m = state.metrics();
    m.db_pool_size.set(i64::from(pool_size));
    m.db_pool_idle.set(i64::from(pool_idle));
    m.db_pool_active.set(i64::from(pool_active));
    m.db_pool_max
        .set(i64::from(state.db_pool_max_connections()));

    // Update circuit breaker state gauges (0=closed, 1=open, 2=half_open)
    m.ai_circuit_breaker_state.set(breaker_state_value(
        state.ai_providers().circuit_breaker().state_name(),
    ));
    if let Some(breaker) = state.email().and_then(|e| e.transport().breaker_state()) {
        m.email_circuit_breaker_state
            .set(breaker_state_value(breaker));
    }

    let output = state.metrics().encode();

    (
        StatusCode::OK,
        [
            ("content-type", "text/plain; version=0.0.4; charset=utf-8"),
            ("cache-control", "no-store"),
        ],
        output,
    )
        .into_response()
}

/// Map circuit breaker state name to numeric gauge value.
fn breaker_state_value(state_name: &str) -> i64 {
    match state_name {
        "closed" => 0,
        "open" => 1,
        "half_open" => 2,
        _ => -1,
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[test]
    fn test_metrics_content_type() {
        // Prometheus expects this exact content type
        let ct = "text/plain; version=0.0.4; charset=utf-8";
        assert!(ct.starts_with("text/plain"));
    }
}
