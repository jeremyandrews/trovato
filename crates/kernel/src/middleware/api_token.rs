//! API token authentication middleware.
//!
//! Checks for `Authorization: Bearer <token>` headers and, if valid,
//! injects the token's user_id into the session so existing handlers
//! work unchanged.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use tower_sessions::Session;
use uuid::Uuid;

use crate::models::api_token::ApiToken;
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

/// Middleware that authenticates via Bearer token.
///
/// If an `Authorization: Bearer <token>` header is present:
/// - Valid token -> injects user_id into session, fires touch_last_used in background
/// - Invalid/expired -> returns 401 JSON error
/// - No header -> passes through (session auth may still work)
///
/// If the session already contains a user_id (cookie auth), that takes precedence
/// and the Bearer token is ignored. This avoids overwriting an existing cookie
/// session with a potentially different token user.
///
/// Note: When no session cookie exists, inserting user_id creates a new server-side
/// session in Redis. This session is bounded by the global TTL (24h inactivity) and
/// the response Set-Cookie header causes the client to reuse it on subsequent
/// requests, so at most one session is created per client, not per request.
pub async fn authenticate_api_token(
    State(state): State<AppState>,
    session: Session,
    request: Request<Body>,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let raw_token = match auth_header {
        Some(v) if v.starts_with("Bearer ") => &v[7..],
        _ => return next.run(request).await,
    };

    // If session already has a user (cookie auth), let it take precedence.
    if let Ok(Some(_)) = session.get::<Uuid>(SESSION_USER_ID).await {
        return next.run(request).await;
    }

    let token = match ApiToken::find_by_token(state.db(), raw_token).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "Invalid or expired API token"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to look up API token");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error": "Internal server error"})),
            )
                .into_response();
        }
    };

    // Inject user_id into session so downstream handlers work unchanged.
    if let Err(e) = session.insert(SESSION_USER_ID, token.user_id).await {
        tracing::error!(error = %e, "failed to inject user_id from API token");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"error": "Internal server error"})),
        )
            .into_response();
    }

    // Update last_used in background
    let pool = state.db().clone();
    let token_id = token.id;
    tokio::spawn(async move {
        if let Err(e) = ApiToken::touch_last_used(&pool, token_id).await {
            tracing::warn!(error = %e, "failed to update API token last_used");
        }
    });

    next.run(request).await
}
