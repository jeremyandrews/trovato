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

/// Session key for remember_me flag.
pub const SESSION_REMEMBER_ME: &str = "remember_me";

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
/// - Checks for account lockout
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

    // Check if account is locked
    match state.lockout().is_locked(&request.username).await {
        Ok(true) => {
            // Get remaining lockout time for user-friendly message
            let remaining = state
                .lockout()
                .get_lockout_remaining(&request.username)
                .await
                .unwrap_or(None);

            let message = if let Some(secs) = remaining {
                format!(
                    "Account temporarily locked. Try again in {} minutes.",
                    (secs / 60) + 1
                )
            } else {
                "Account temporarily locked. Try again later.".to_string()
            };

            return Err((StatusCode::TOO_MANY_REQUESTS, Json(AuthError { error: message })));
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!(error = %e, "failed to check lockout status");
            // Continue with login attempt even if lockout check fails
        }
    }

    // Find user by username
    let user = match User::find_by_name(state.db(), &request.username).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            // Record failed attempt even for non-existent users (prevent enumeration)
            let _ = state.lockout().record_failed_attempt(&request.username).await;
            return Err(auth_error());
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during login");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Check if user is active
    if !user.is_active() {
        let _ = state.lockout().record_failed_attempt(&request.username).await;
        return Err(auth_error());
    }

    // Verify password
    if !user.verify_password(&request.password) {
        // Record failed attempt
        match state.lockout().record_failed_attempt(&request.username).await {
            Ok((locked, remaining)) => {
                if locked {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(AuthError {
                            error: "Account temporarily locked due to too many failed attempts. Try again in 15 minutes.".to_string(),
                        }),
                    ));
                } else {
                    tracing::info!(
                        username = %request.username,
                        remaining_attempts = remaining,
                        "failed login attempt"
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to record failed attempt");
            }
        }
        return Err(auth_error());
    }

    // Successful login - clear any failed attempts
    let _ = state.lockout().clear_attempts(&request.username).await;

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

    // Store remember_me preference in session
    session
        .insert(SESSION_REMEMBER_ME, request.remember_me)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert remember_me into session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

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
