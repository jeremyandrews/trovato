//! Admin routes (stage switching, etc.).

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::routes::auth::SESSION_ACTIVE_STAGE;
use crate::state::AppState;

/// Stage switch request.
#[derive(Debug, Deserialize)]
pub struct StageSwitchRequest {
    /// Stage ID to switch to. None means "live" (production).
    pub stage_id: Option<String>,
}

/// Stage switch response.
#[derive(Debug, Serialize)]
pub struct StageSwitchResponse {
    pub success: bool,
    pub active_stage: Option<String>,
}

/// Error response.
#[derive(Debug, Serialize)]
pub struct AdminError {
    pub error: String,
}

/// Switch the active stage for the current session.
///
/// POST /admin/stage/switch
async fn switch_stage(
    session: Session,
    Json(request): Json<StageSwitchRequest>,
) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
    // TODO: Verify user has permission to switch stages
    // For now, any authenticated user can switch

    session
        .insert(SESSION_ACTIVE_STAGE, request.stage_id.clone())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to update active_stage in session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminError {
                    error: "Failed to switch stage".to_string(),
                }),
            )
        })?;

    tracing::info!(stage = ?request.stage_id, "stage switched");

    Ok(Json(StageSwitchResponse {
        success: true,
        active_stage: request.stage_id,
    }))
}

/// Get the current active stage.
///
/// GET /admin/stage/current (via session extractor in handlers)
async fn get_current_stage(
    session: Session,
) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
    let active_stage: Option<String> = session
        .get(SESSION_ACTIVE_STAGE)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to get active_stage from session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminError {
                    error: "Failed to get stage".to_string(),
                }),
            )
        })?
        .flatten();

    Ok(Json(StageSwitchResponse {
        success: true,
        active_stage,
    }))
}

/// Create the admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/stage/switch", post(switch_stage))
        .route("/admin/stage/current", axum::routing::get(get_current_stage))
}
