//! API token management routes.
//!
//! All endpoints require an authenticated session (cookie or existing Bearer token).
//!
//! `/user/tokens` is the page a person manages their own tokens on. It renders
//! the list and the forms; creating and revoking go through the JSON endpoints
//! below, called by `static/js/api-tokens.js` with the page's CSRF token, so the
//! page and an API client use one contract. The raw token exists only in the
//! create response: the page shows it once and the server never renders it.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::error::AppError;
use crate::models::api_token::{ApiToken, MAX_TOKENS_PER_USER};
use crate::routes::helpers::{require_csrf_header, require_login_json};
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

/// POST /api/tokens — Create a new API token.
async fn create_token(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), AppError> {
    let user_id = require_login_json(&state, &session).await?.id;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;

    let name = body.name.trim();
    if name.is_empty() || name.len() > 255 {
        return Err(AppError::bad_request("Token name must be 1-255 characters"));
    }

    // Enforce per-user token limit
    let count = ApiToken::count_for_user(state.db(), user_id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "count API tokens"))?;

    if count >= MAX_TOKENS_PER_USER {
        return Err(AppError::bad_request(format!(
            "Maximum of {MAX_TOKENS_PER_USER} tokens per user"
        )));
    }

    let expires_at = body
        .expires_in_days
        .map(|days| Utc::now() + Duration::days(i64::from(days)));

    let (token_record, raw_token) = ApiToken::create(state.db(), user_id, name, expires_at)
        .await
        .map_err(|e| AppError::internal_ctx(e, "create API token"))?;

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
) -> Result<Json<Vec<TokenListItem>>, AppError> {
    let user_id = require_login_json(&state, &session).await?.id;

    let tokens = ApiToken::list_for_user(state.db(), user_id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "list API tokens"))?;

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
) -> Result<StatusCode, AppError> {
    let user_id = require_login_json(&state, &session).await?.id;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;

    let deleted = ApiToken::delete(state.db(), id, user_id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "delete API token"))?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found_id("token", id))
    }
}

/// One token as the page lists it, with its times already formatted.
#[derive(Debug, Serialize)]
struct TokenRow {
    id: Uuid,
    name: String,
    created: String,
    last_used: Option<String>,
    expires_at: Option<String>,
}

fn page_time(t: chrono::DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// GET /user/tokens — the signed-in user's API tokens, and the forms to create
/// and revoke them.
async fn tokens_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match crate::routes::helpers::require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let csrf_token = crate::form::csrf::generate_csrf_token(&session).await;
    let tokens = match ApiToken::list_for_user(state.db(), user.id).await {
        Ok(tokens) => tokens,
        Err(e) => {
            tracing::error!(error = %e, "failed to list API tokens for the token page");
            return crate::routes::helpers::render_server_error("Could not load your API tokens.");
        }
    };
    let rows: Vec<TokenRow> = tokens
        .into_iter()
        .map(|t| TokenRow {
            id: t.id,
            name: t.name,
            created: page_time(t.created),
            last_used: t.last_used.map(page_time),
            expires_at: t.expires_at.map(page_time),
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("user", &user);
    context.insert("tokens", &rows);
    context.insert("max_tokens", &MAX_TOKENS_PER_USER);
    crate::routes::helpers::inject_site_context(&state, &session, &mut context, "/user/tokens")
        .await;

    match state.theme().tera().render("user/tokens.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the token page");
            crate::routes::helpers::render_server_error("Could not render the token page.")
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/tokens", get(tokens_page))
        .route("/api/tokens", post(create_token).get(list_tokens))
        .route("/api/tokens/{id}", delete(delete_token))
}
