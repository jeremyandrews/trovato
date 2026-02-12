//! Health check endpoint.
//!
//! Returns 200 OK if both PostgreSQL and Redis are reachable,
//! 503 Service Unavailable otherwise.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::state::AppState;

/// Health check response.
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    postgres: bool,
    redis: bool,
}

/// Health check handler.
async fn health_check(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let (postgres, redis) = tokio::join!(state.postgres_healthy(), state.redis_healthy());

    let status = if postgres && redis {
        "healthy"
    } else {
        "unhealthy"
    };

    let status_code = if postgres && redis {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status_code,
        Json(HealthResponse {
            status,
            postgres,
            redis,
        }),
    )
}

/// Create the health check router.
pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health_check))
}
