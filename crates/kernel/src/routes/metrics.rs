//! Prometheus metrics endpoint.

use axum::Router;
use axum::extract::State;
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
/// Returns metrics in Prometheus text exposition format.
async fn metrics(State(state): State<AppState>) -> Response {
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

    let output = state.metrics().encode();

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        output,
    )
        .into_response()
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
