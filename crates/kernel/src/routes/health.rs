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
    #[serde(skip_serializing_if = "Option::is_none")]
    pool: Option<PoolHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    services: Option<crate::state::HealthReport>,
}

/// Database connection pool health statistics.
#[derive(Debug, Serialize)]
struct PoolHealth {
    /// Current number of connections in the pool.
    size: u32,
    /// Number of idle (unused) connections.
    idle: u32,
    /// Number of actively used connections.
    active: u32,
    /// Pool utilization as a percentage (active / size * 100).
    utilization_pct: u32,
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

    let pool_size = state.db().size();
    let pool_idle = state.db().num_idle() as u32;
    let pool_active = pool_size.saturating_sub(pool_idle);
    let utilization_pct = if pool_size > 0 {
        (pool_active * 100) / pool_size
    } else {
        0
    };
    let pool = Some(PoolHealth {
        size: pool_size,
        idle: pool_idle,
        active: pool_active,
        utilization_pct,
    });

    // Build the full service health report (includes circuit breaker states, etc.)
    let services = Some(state.health_report().await);

    (
        status_code,
        Json(HealthResponse {
            status,
            postgres,
            redis,
            pool,
            services,
        }),
    )
}

/// Create the health check router.
pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health_check))
}
