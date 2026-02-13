//! Cron route handlers.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use tower_sessions::Session;
use tracing::info;

use crate::cron::CronResult;
use crate::models::User;
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

/// Create the cron router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/cron/{key}", post(run_cron))
        .route("/cron/status", get(cron_status))
}

/// Cron run response.
#[derive(Debug, Serialize)]
pub struct CronResponse {
    pub status: String,
    pub tasks: Option<Vec<String>>,
    pub duration_ms: Option<u64>,
    pub message: Option<String>,
}

/// Run cron tasks (protected by secret key).
async fn run_cron(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Response {
    // Validate cron key
    let expected_key = std::env::var("CRON_KEY").unwrap_or_else(|_| "default-cron-key".to_string());
    if key != expected_key {
        info!(provided_key = %key, "invalid cron key");
        return (
            StatusCode::FORBIDDEN,
            Json(CronResponse {
                status: "error".to_string(),
                tasks: None,
                duration_ms: None,
                message: Some("Invalid cron key".to_string()),
            }),
        )
            .into_response();
    }

    // Run cron
    info!("cron triggered via HTTP");
    let result = state.cron().run().await;

    match result {
        CronResult::Completed {
            tasks_run,
            duration_ms,
        } => (
            StatusCode::OK,
            Json(CronResponse {
                status: "completed".to_string(),
                tasks: Some(tasks_run),
                duration_ms: Some(duration_ms),
                message: None,
            }),
        )
            .into_response(),
        CronResult::Skipped => (
            StatusCode::OK,
            Json(CronResponse {
                status: "skipped".to_string(),
                tasks: None,
                duration_ms: None,
                message: Some("Another instance is running cron".to_string()),
            }),
        )
            .into_response(),
        CronResult::Failed(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CronResponse {
                status: "failed".to_string(),
                tasks: None,
                duration_ms: None,
                message: Some(error),
            }),
        )
            .into_response(),
    }
}

/// Cron status response.
#[derive(Debug, Serialize)]
pub struct CronStatusResponse {
    pub last_run: Option<LastRunInfo>,
    pub queue_lengths: QueueLengths,
}

/// Last run information.
#[derive(Debug, Serialize)]
pub struct LastRunInfo {
    pub timestamp: i64,
    pub hostname: String,
    pub result: String,
    pub time_ago: String,
}

/// Queue lengths.
#[derive(Debug, Serialize)]
pub struct QueueLengths {
    pub email_send: u64,
    pub search_reindex: u64,
}

/// Check if user is admin, return user or redirect to login.
async fn require_admin(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<uuid::Uuid> = session
        .get(SESSION_USER_ID)
        .await
        .ok()
        .flatten();

    if let Some(id) = user_id {
        if let Ok(Some(user)) = User::find_by_id(state.db(), id).await {
            if user.is_admin {
                return Ok(user);
            }
            // User exists but is not admin
            return Err((StatusCode::FORBIDDEN, "Admin access required").into_response());
        }
    }

    Err(Redirect::to("/user/login").into_response())
}

/// Get cron status (admin only).
async fn cron_status(
    State(state): State<AppState>,
    session: Session,
) -> Response {
    // Check admin permission
    if let Err(e) = require_admin(&state, &session).await {
        return e;
    }

    // Get last run info
    let last_run = match state.cron().last_run().await {
        Ok(Some(run)) => {
            let now = chrono::Utc::now().timestamp();
            let seconds_ago = now - run.timestamp;
            let time_ago = if seconds_ago < 60 {
                format!("{} seconds ago", seconds_ago)
            } else if seconds_ago < 3600 {
                format!("{} minutes ago", seconds_ago / 60)
            } else if seconds_ago < 86400 {
                format!("{} hours ago", seconds_ago / 3600)
            } else {
                format!("{} days ago", seconds_ago / 86400)
            };

            Some(LastRunInfo {
                timestamp: run.timestamp,
                hostname: run.hostname,
                result: run.result,
                time_ago,
            })
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "failed to get cron last run");
            None
        }
    };

    // Get queue lengths
    use crate::cron::Queue;
    let queue = state.cron().queue();
    let email_send = queue.len("email:send").await.unwrap_or(0);
    let search_reindex = queue.len("search:reindex").await.unwrap_or(0);

    Json(CronStatusResponse {
        last_run,
        queue_lengths: QueueLengths {
            email_send,
            search_reindex,
        },
    })
    .into_response()
}
