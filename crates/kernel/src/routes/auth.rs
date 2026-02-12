//! Authentication routes (login, logout).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use tracing::info;

use crate::models::User;
use crate::state::AppState;

/// Session key for storing the authenticated user ID.
pub const SESSION_USER_ID: &str = "user_id";

/// Session key for storing the active stage.
pub const SESSION_ACTIVE_STAGE: &str = "active_stage";

/// Login request body.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: bool,
}

/// Login response.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub message: String,
}

/// Error response for authentication failures.
#[derive(Debug, Serialize)]
pub struct AuthError {
    pub error: String,
}

/// Login handler.
///
/// POST /user/login
/// - Verifies username and password
/// - Creates session on success
/// - Updates login/access timestamps
/// - Returns 401 on failure without revealing which field was wrong
async fn login(
    State(state): State<AppState>,
    session: Session,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
    // Generic error message that doesn't reveal which field was wrong
    let auth_error = || {
        (
            StatusCode::UNAUTHORIZED,
            Json(AuthError {
                error: "Invalid username or password".to_string(),
            }),
        )
    };

    // Find user by username
    let user = User::find_by_name(state.db(), &request.username)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "database error during login");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?
        .ok_or_else(auth_error)?;

    // Check if user is active
    if !user.is_active() {
        return Err(auth_error());
    }

    // Verify password
    if !user.verify_password(&request.password) {
        return Err(auth_error());
    }

    // Update login timestamp
    if let Err(e) = User::touch_login(state.db(), user.id).await {
        tracing::warn!(error = %e, user_id = %user.id, "failed to update login timestamp");
    }

    // Create session
    session
        .insert(SESSION_USER_ID, user.id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert user_id into session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

    // Set default stage (None = live)
    session
        .insert(SESSION_ACTIVE_STAGE, Option::<String>::None)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert active_stage into session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

    // Handle remember_me by extending session expiry
    // Note: tower-sessions handles this via configuration
    // For now we just log it; actual implementation would adjust session config
    if request.remember_me {
        info!(user_id = %user.id, "user logged in with remember_me");
    } else {
        info!(user_id = %user.id, "user logged in");
    }

    Ok(Json(LoginResponse {
        success: true,
        message: "Login successful".to_string(),
    }))
}

/// Logout handler.
///
/// GET /user/logout
/// - Deletes session from Redis
/// - Clears session cookie
async fn logout(session: Session) -> Result<Json<LoginResponse>, (StatusCode, Json<AuthError>)> {
    session.delete().await.map_err(|e| {
        tracing::error!(error = %e, "failed to delete session");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AuthError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    Ok(Json(LoginResponse {
        success: true,
        message: "Logout successful".to_string(),
    }))
}

/// Create the auth router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/login", post(login))
        .route("/user/logout", get(logout))
}
