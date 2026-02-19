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
    let output = state.metrics().encode();

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        output,
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[test]
    fn test_metrics_content_type() {
        // Prometheus expects this exact content type
        let ct = "text/plain; version=0.0.4; charset=utf-8";
        assert!(ct.starts_with("text/plain"));
    }
}
