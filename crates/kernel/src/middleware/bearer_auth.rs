//! Bearer token authentication middleware.
//!
//! Checks `Authorization: Bearer <token>` headers, verifies JWT,
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
/// If a valid Bearer JWT is present, sets the user context in request
/// extensions. If no bearer value is present, passes through without
/// modification. If a JWT is present but invalid or revoked, returns 401.
///
/// Two kinds of credential share the `Authorization: Bearer` header: OAuth
/// access tokens, which are JWTs, and kernel API tokens, which are opaque hex
/// strings checked by [`crate::middleware::authenticate_api_token`]. This
/// middleware runs first, so it must only claim the values that are its own. A
/// value that does not have the shape of a compact JWS, and any value at all
/// while OAuth is disabled, passes through untouched for the API token
/// middleware to accept or refuse. Answering 401 to those here is what once made
/// every API token unusable in production.
pub async fn authenticate_bearer_token(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    // A probe or a scrape carries an operator's token, not a user's credential.
    if crate::routes::health::carries_operator_token(request.method(), request.uri().path()) {
        return next.run(request).await;
    }
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

    // Not a JWT: an API token, or something the API token middleware will
    // refuse. Either way it is not this middleware's to judge.
    if !looks_like_compact_jws(token) {
        return next.run(request).await;
    }

    // OAuth disabled: no JWT can be valid here, and the value may still be an
    // API token, so leave the decision to the API token middleware.
    let Some(oauth) = state.oauth() else {
        return next.run(request).await;
    };

    // Verify the JWT (must be an access token, not a refresh token)
    let claims = match oauth.verify_access_token(token) {
        Ok(c) => c,
        Err(e) => {
            debug!(error = %e, "invalid bearer token");
            return (
                [("WWW-Authenticate", "Bearer error=\"invalid_token\"")],
                crate::error::AppError::unauthorized("Invalid token"),
            )
                .into_response();
        }
    };

    // Check revocation
    match oauth.is_revoked(&claims.jti, state.redis()).await {
        Ok(true) => {
            debug!(jti = %claims.jti, "bearer token revoked");
            return (
                [("WWW-Authenticate", "Bearer error=\"invalid_token\"")],
                crate::error::AppError::unauthorized("Token has been revoked"),
            )
                .into_response();
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(error = %e, "failed to check token revocation; denying request");
            return crate::error::AppError::with_status(
                StatusCode::SERVICE_UNAVAILABLE,
                "Unable to verify token status",
            )
            .into_response();
        }
    }

    // Parse user ID from claims
    let Ok(user_id) = claims.sub.parse::<Uuid>() else {
        debug!(sub = %claims.sub, "invalid user ID in token");
        return crate::error::AppError::unauthorized("Invalid token subject").into_response();
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

/// Whether `value` has the shape of a compact JWS: three non-empty,
/// dot-separated segments of base64url characters.
///
/// This is a shape test, not a verification. It only decides which middleware
/// judges the value; a value that passes still goes through full JWT
/// verification below. An API token (64 hex characters, no dots) never passes.
fn looks_like_compact_jws(value: &str) -> bool {
    let mut segments = 0;
    for segment in value.split('.') {
        segments += 1;
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
        {
            return false;
        }
    }
    segments == 3
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

#[cfg(test)]
mod tests {
    use super::looks_like_compact_jws;

    #[test]
    fn a_jwt_is_recognised_by_shape() {
        assert!(looks_like_compact_jws(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl"
        ));
        assert!(looks_like_compact_jws("a.b.c"));
    }

    #[test]
    fn an_api_token_and_other_values_are_not_jwts() {
        // The shape `ApiToken` mints: 64 hex characters.
        assert!(!looks_like_compact_jws(&"ab12".repeat(16)));
        assert!(!looks_like_compact_jws(""));
        assert!(!looks_like_compact_jws("a.b"));
        assert!(!looks_like_compact_jws("a.b.c.d"));
        assert!(!looks_like_compact_jws("a..c"));
        assert!(!looks_like_compact_jws("a.b c.d"));
    }
}
