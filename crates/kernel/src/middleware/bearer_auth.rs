//! Bearer token authentication middleware.
//!
//! Checks Authorization: Bearer <token> headers, verifies JWT,
//! checks Redis revocation blocklist, and sets user context.

use axum::{
    body::Body,
    extract::State,
    http::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::debug;
use uuid::Uuid;

use crate::services::oauth::CLIENT_CREDENTIALS_USER_ID;
use crate::state::AppState;

/// Middleware to authenticate Bearer JWT tokens.
///
/// If a valid Bearer token is present, sets the user context in request
/// extensions. If no token is present, passes through without modification.
/// If an invalid token is present, returns 401.
pub async fn authenticate_bearer_token(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());

    let Some(auth_header) = auth_header else {
        return next.run(request).await;
    };

    let Some(token) = auth_header.strip_prefix("Bearer ") else {
        return next.run(request).await;
    };

    // Check if OAuth service is available
    let Some(oauth) = state.oauth() else {
        // Bearer token was presented but OAuth is disabled — reject
        return (
            StatusCode::UNAUTHORIZED,
            [("WWW-Authenticate", "Bearer error=\"invalid_token\"")],
            "OAuth service unavailable",
        )
            .into_response();
    };

    // Verify the JWT (must be an access token, not a refresh token)
    let claims = match oauth.verify_access_token(token) {
        Ok(c) => c,
        Err(e) => {
            debug!(error = %e, "invalid bearer token");
            return (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", "Bearer error=\"invalid_token\"")],
                "Invalid token",
            )
                .into_response();
        }
    };

    // Check revocation
    match oauth.is_revoked(&claims.jti, state.redis()).await {
        Ok(true) => {
            debug!(jti = %claims.jti, "bearer token revoked");
            return (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", "Bearer error=\"invalid_token\"")],
                "Token has been revoked",
            )
                .into_response();
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(error = %e, "failed to check token revocation; denying request");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Unable to verify token status",
            )
                .into_response();
        }
    }

    // Parse user ID from claims
    let Ok(user_id) = claims.sub.parse::<Uuid>() else {
        debug!(sub = %claims.sub, "invalid user ID in token");
        return (StatusCode::UNAUTHORIZED, "Invalid token subject").into_response();
    };

    // Detect client_credentials tokens (machine-to-machine, no real user).
    let is_client_credentials = user_id == CLIENT_CREDENTIALS_USER_ID;

    // Store bearer auth info in request extensions for handlers to use
    request.extensions_mut().insert(BearerAuth {
        user_id,
        client_id: claims.client_id,
        scope: claims.scope,
        jti: claims.jti,
        is_client_credentials,
    });

    next.run(request).await
}

/// Bearer authentication info extracted from a valid JWT.
#[derive(Debug, Clone)]
pub struct BearerAuth {
    pub user_id: Uuid,
    pub client_id: String,
    pub scope: String,
    pub jti: String,
    /// True for client_credentials tokens (machine-to-machine, no real user).
    /// Handlers should check this before treating `user_id` as a real user.
    pub is_client_credentials: bool,
}
