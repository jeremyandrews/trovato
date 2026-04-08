//! Password reset routes.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tracing::info;

use crate::error::AppError;
use crate::models::password_reset::PasswordResetToken;
use crate::routes::helpers::{JsonSuccess, validate_password};
use crate::state::AppState;
use crate::tap::UserContext;

/// Password reset request (step 1: request reset).
#[derive(Debug, Deserialize)]
pub struct RequestResetInput {
    pub email: String,
}

/// New password input (step 2: set new password).
#[derive(Debug, Deserialize)]
pub struct SetPasswordInput {
    pub password: String,
}

/// Request a password reset.
///
/// POST /user/password-reset
/// Always returns success (security: don't reveal if email exists).
async fn request_reset(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<RequestResetInput>,
) -> Json<JsonSuccess> {
    let client_id = crate::middleware::get_client_id(None, &headers);
    // Try to find user by email
    match state.users().find_by_mail(&input.email).await {
        Ok(Some(user)) => {
            // Create reset token
            match PasswordResetToken::create(state.db(), user.id).await {
                Ok((_, plain_token)) => {
                    // Send email if SMTP is configured, otherwise log
                    if let Some(email_service) = state.email() {
                        let site_name = crate::models::SiteConfig::get(state.db(), "site_name")
                            .await
                            .ok()
                            .flatten()
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .unwrap_or_else(|| "Trovato".to_string());

                        if let Err(e) = email_service
                            .send_password_reset(&input.email, &plain_token, &site_name)
                            .await
                        {
                            tracing::error!(error = %e, "failed to send password reset email");
                            // Fall back to logging
                            tracing::debug!(
                                reset_url = format!("/user/password-reset/{}", plain_token),
                                "Reset URL (email send failed)"
                            );
                        } else {
                            info!(
                                user_id = %user.id,
                                "password reset email sent"
                            );
                        }
                    } else {
                        // SMTP not configured — log the token for development
                        tracing::debug!(
                            user_id = %user.id,
                            token = %plain_token,
                            "Password reset requested (SMTP not configured, token logged)"
                        );
                        tracing::debug!(
                            reset_url = format!("/user/password-reset/{}", plain_token),
                            "Reset URL for testing"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to create reset token");
                }
            }
        }
        Ok(None) => {
            // User not found - log but don't reveal to client
            info!(client = %client_id, "password reset requested for non-existent email");
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during password reset request");
        }
    }

    // Always return success (security)
    Json(JsonSuccess {
        success: true,
        message: "If an account with that email exists, a reset link has been sent.".to_string(),
    })
}

/// Validate a reset token (show reset form).
///
/// GET /user/password-reset/:token
async fn validate_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<JsonSuccess>, AppError> {
    let reset_token = PasswordResetToken::find_valid(state.db(), &token)
        .await
        .map_err(|e| AppError::internal_ctx(e, "validate reset token"))?;

    if reset_token.is_some() {
        Ok(Json(JsonSuccess {
            success: true,
            message: "Token is valid. You may set a new password.".to_string(),
        }))
    } else {
        Err(AppError::bad_request("Invalid or expired reset token"))
    }
}

/// Set new password using reset token.
///
/// POST /user/password-reset/:token
async fn set_password(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(input): Json<SetPasswordInput>,
) -> Result<Json<JsonSuccess>, AppError> {
    // Validate password
    if let Err(msg) = validate_password(&input.password) {
        return Err(AppError::bad_request(msg.to_string()));
    }

    // Find and validate token
    let reset_token = PasswordResetToken::find_valid(state.db(), &token)
        .await
        .map_err(|e| AppError::internal_ctx(e, "validate reset token"))?
        .ok_or_else(|| AppError::bad_request("Invalid or expired reset token"))?;

    // Update the password (anonymous context — user is not logged in)
    let anon = UserContext::anonymous();
    state
        .users()
        .update_password(reset_token.user_id, &input.password, &anon)
        .await
        .map_err(|e| AppError::internal_ctx(e, "update password"))?;

    // Mark token as used
    PasswordResetToken::mark_used(state.db(), reset_token.id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "mark reset token as used"))?;

    // Invalidate any other tokens for this user
    if let Err(e) =
        PasswordResetToken::invalidate_user_tokens(state.db(), reset_token.user_id).await
    {
        tracing::warn!(error = %e, user_id = %reset_token.user_id, "failed to invalidate password reset tokens");
    }

    info!(user_id = %reset_token.user_id, "password reset completed");

    Ok(Json(JsonSuccess {
        success: true,
        message: "Password has been reset. You may now log in.".to_string(),
    }))
}

/// Create the password reset router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/password-reset", post(request_reset))
        .route(
            "/user/password-reset/{token}",
            get(validate_token).post(set_password),
        )
}
