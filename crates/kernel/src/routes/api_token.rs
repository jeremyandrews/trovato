//! API token management routes.
//!
//! All endpoints require an authenticated session (cookie or existing Bearer token).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, post},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::models::api_token::{ApiToken, MAX_TOKENS_PER_USER};
use crate::routes::auth::SESSION_USER_ID;
use crate::routes::helpers::require_csrf_header;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    pub name: String,
    /// Optional expiration in days from now. If omitted, the token never expires.
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CreateTokenResponse {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TokenListItem {
    pub id: Uuid,
    pub name: String,
    pub created: i64,
    pub last_used: Option<i64>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// POST /api/tokens — Create a new API token.
async fn create_token(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), (StatusCode, Json<ErrorResponse>)> {
    let user_id: Uuid = session
        .get(SESSION_USER_ID)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Authentication required".to_string(),
                }),
            )
        })?;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(ErrorResponse {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    let name = body.name.trim();
    if name.is_empty() || name.len() > 255 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Token name must be 1-255 characters".to_string(),
            }),
        ));
    }

    // Enforce per-user token limit
    let count = ApiToken::count_for_user(state.db(), user_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to count API tokens");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to create token".to_string(),
                }),
            )
        })?;

    if count >= MAX_TOKENS_PER_USER {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Maximum of {MAX_TOKENS_PER_USER} tokens per user"),
            }),
        ));
    }

    let expires_at = body
        .expires_in_days
        .map(|days| Utc::now() + Duration::days(i64::from(days)));

    let (token_record, raw_token) = ApiToken::create(state.db(), user_id, name, expires_at)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create API token");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to create token".to_string(),
                }),
            )
        })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            id: token_record.id,
            name: token_record.name,
            token: raw_token,
            expires_at: token_record.expires_at.map(|t| t.timestamp()),
        }),
    ))
}

/// GET /api/tokens — List the current user's tokens.
async fn list_tokens(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Vec<TokenListItem>>, (StatusCode, Json<ErrorResponse>)> {
    let user_id: Uuid = session
        .get(SESSION_USER_ID)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Authentication required".to_string(),
                }),
            )
        })?;

    let tokens = ApiToken::list_for_user(state.db(), user_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list API tokens");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list tokens".to_string(),
                }),
            )
        })?;

    let items: Vec<TokenListItem> = tokens
        .into_iter()
        .map(|t| TokenListItem {
            id: t.id,
            name: t.name,
            created: t.created.timestamp(),
            last_used: t.last_used.map(|ts| ts.timestamp()),
            expires_at: t.expires_at.map(|ts| ts.timestamp()),
        })
        .collect();

    Ok(Json(items))
}

/// DELETE /api/tokens/{id} — Revoke a token.
async fn delete_token(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let user_id: Uuid = session
        .get(SESSION_USER_ID)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Authentication required".to_string(),
                }),
            )
        })?;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(ErrorResponse {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    let deleted = ApiToken::delete(state.db(), id, user_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to delete API token");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to delete token".to_string(),
                }),
            )
        })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Token not found".to_string(),
            }),
        ))
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tokens", post(create_token).get(list_tokens))
        .route("/api/tokens/{id}", delete(delete_token))
}
