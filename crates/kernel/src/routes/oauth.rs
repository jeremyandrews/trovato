//! OAuth2 routes.
//!
//! Provides /oauth/authorize, /oauth/token, and /oauth/revoke endpoints.
//! Implements RFC 6749 (OAuth 2.0) with PKCE (RFC 7636).

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::services::oauth::CLIENT_CREDENTIALS_USER_ID;
use crate::state::AppState;

/// Token request payload (application/x-www-form-urlencoded per RFC 6749 §4.1.3).
#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub scope: String,
    /// PKCE code verifier (RFC 7636).
    #[serde(default)]
    pub code_verifier: String,
}

/// Revoke request payload (application/x-www-form-urlencoded per RFC 7009 §2.1).
#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    pub token: String,
    /// Client ID — required to verify token ownership (RFC 7009 §2.1).
    #[serde(default)]
    pub client_id: String,
    /// Client secret — required for confidential clients.
    #[serde(default)]
    pub client_secret: String,
}

/// Maximum allowed length for scope strings to prevent memory abuse.
const MAX_SCOPE_LENGTH: usize = 1000;

/// Maximum allowed length for PKCE code_verifier (RFC 7636 §4.1: 43-128 chars,
/// but we allow up to 256 for safety margin).
const MAX_CODE_VERIFIER_LENGTH: usize = 256;

/// Validate that a scope string contains only safe characters and is bounded in length.
///
/// Scope tokens are defined in RFC 6749 §3.3 as: `%x21 / %x23-5B / %x5D-7E`
/// (printable ASCII except `"` and `\`, separated by spaces).
fn validate_scope(scope: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    if scope.len() > MAX_SCOPE_LENGTH {
        return false;
    }
    scope
        .bytes()
        .all(|b| b == 0x20 || b == 0x21 || (0x23..=0x5B).contains(&b) || (0x5D..=0x7E).contains(&b))
}

/// Build token response headers per RFC 6749 §5.1.
fn token_response_headers() -> [(axum::http::HeaderName, &'static str); 2] {
    [
        (axum::http::header::CACHE_CONTROL, "no-store"),
        (axum::http::header::PRAGMA, "no-cache"),
    ]
}

/// Create the OAuth routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
}

/// GET /oauth/authorize — authorization consent page.
async fn authorize(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(oauth) = state.oauth() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not enabled").into_response();
    };

    // Validate response_type (RFC 6749 §4.1.1 — REQUIRED, MUST be "code")
    let response_type = params
        .get("response_type")
        .map(|s| s.as_str())
        .unwrap_or("");
    if response_type != "code" {
        return (StatusCode::BAD_REQUEST, "response_type must be 'code'").into_response();
    }

    let user_id = match session.get::<Uuid>("user_id").await.ok().flatten() {
        Some(id) => id,
        None => return (StatusCode::UNAUTHORIZED, "Authentication required").into_response(),
    };

    let client_id = params.get("client_id").map(|s| s.as_str()).unwrap_or("");
    let redirect_uri = params.get("redirect_uri").map(|s| s.as_str()).unwrap_or("");
    let scope = params.get("scope").map(|s| s.as_str()).unwrap_or("");
    let state_param = params.get("state").map(|s| s.as_str()).unwrap_or("");

    // Validate scope characters (RFC 6749 §3.3)
    if !validate_scope(scope) {
        return (StatusCode::BAD_REQUEST, "Invalid scope characters").into_response();
    }

    // PKCE parameters (RFC 7636)
    let code_challenge = params.get("code_challenge").map(|s| s.as_str());
    let code_challenge_method = params.get("code_challenge_method").map(|s| s.as_str());

    // Validate code_challenge_method if provided (only S256 accepted per RFC 7636 §4.2)
    if let Some(method) = code_challenge_method
        && method != "S256"
    {
        return (
            StatusCode::BAD_REQUEST,
            "code_challenge_method must be 'S256'",
        )
            .into_response();
    }

    // If code_challenge_method is provided, code_challenge must be too
    if code_challenge_method.is_some() && code_challenge.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "code_challenge required when code_challenge_method is provided",
        )
            .into_response();
    }

    // Validate client
    let client = match oauth.find_client(client_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return (StatusCode::BAD_REQUEST, "Unknown client").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to find OAuth client");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    // Validate redirect_uri
    let redirect_uris: Vec<String> =
        serde_json::from_value(client.redirect_uris.clone()).unwrap_or_default();

    // RFC 6749 §3.1.2.3: If multiple redirect URIs are registered, the client
    // MUST include the redirect_uri parameter in the authorization request.
    if redirect_uri.is_empty() && redirect_uris.len() > 1 {
        return (
            StatusCode::BAD_REQUEST,
            "redirect_uri required when client has multiple registered URIs",
        )
            .into_response();
    }

    if !redirect_uris.contains(&redirect_uri.to_string()) {
        return (StatusCode::BAD_REQUEST, "Invalid redirect_uri").into_response();
    }

    // Public clients MUST use PKCE
    if !client.is_confidential() && code_challenge.is_none() {
        return (StatusCode::BAD_REQUEST, "PKCE required for public clients").into_response();
    }

    // Filter the requested scope to only scopes this client is authorized for.
    // This ensures the auth code (and eventually the token) never contains
    // scopes beyond the client's allowlist.
    let granted_scope = client.filter_scope(scope);

    // Generate opaque authorization code (stored in Redis, 60s TTL, single-use)
    let code = match oauth
        .create_authorization_code(
            user_id,
            client_id,
            &granted_scope,
            redirect_uri,
            code_challenge,
            code_challenge_method,
            state.redis(),
        )
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to create auth code");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate code").into_response();
        }
    };

    // Build redirect URL with proper query parameter handling
    let separator = if redirect_uri.contains('?') { "&" } else { "?" };
    let mut redirect = format!(
        "{}{}code={}",
        redirect_uri,
        separator,
        urlencoding::encode(&code)
    );

    // Pass through state parameter (RFC 6749 §4.1.2 — REQUIRED if provided by client)
    if !state_param.is_empty() {
        redirect = format!("{}&state={}", redirect, urlencoding::encode(state_param));
    }

    // Sanitize redirect to prevent CRLF injection into Location header.
    let safe_redirect: String = redirect
        .chars()
        .filter(|c| *c != '\r' && *c != '\n')
        .collect();

    (StatusCode::FOUND, [("Location", safe_redirect)]).into_response()
}

/// POST /oauth/token — token endpoint.
///
/// Accepts `application/x-www-form-urlencoded` per RFC 6749 §4.1.3.
async fn token(
    State(state): State<AppState>,
    axum::Form(payload): axum::Form<TokenRequest>,
) -> impl IntoResponse {
    let Some(oauth) = state.oauth() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not enabled").into_response();
    };

    // Validate scope characters (RFC 6749 §3.3)
    if !validate_scope(&payload.scope) {
        return (StatusCode::BAD_REQUEST, "Invalid scope characters").into_response();
    }

    let headers = token_response_headers();

    match payload.grant_type.as_str() {
        "client_credentials" => {
            // Validate client credentials
            let client = match oauth
                .validate_client_credentials(&payload.client_id, &payload.client_secret)
                .await
            {
                Ok(Some(c)) => c,
                Ok(None) => {
                    return (StatusCode::UNAUTHORIZED, "Invalid client credentials")
                        .into_response();
                }
                Err(e) => {
                    tracing::warn!(error = %e, "client validation failed");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
                }
            };

            // Validate grant_type is allowed
            if !client.supports_grant_type("client_credentials") {
                return (
                    StatusCode::BAD_REQUEST,
                    "Grant type not allowed for this client",
                )
                    .into_response();
            }

            // Filter scope to only scopes the client is authorized for
            let granted_scope = client.filter_scope(&payload.scope);

            // Create token for the client (no user context, no refresh token per RFC 6749 §4.4.3).
            // Uses a dedicated sentinel UUID to avoid colliding with ANONYMOUS_USER_ID.
            let response = match oauth.create_access_token(
                CLIENT_CREDENTIALS_USER_ID,
                &client.client_id,
                &granted_scope,
                false,
            ) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "token creation failed");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Token creation failed")
                        .into_response();
                }
            };

            (headers, axum::Json(response)).into_response()
        }
        "authorization_code" => {
            // Validate code_verifier length (RFC 7636 §4.1: 43-128 ASCII chars)
            if payload.code_verifier.len() > MAX_CODE_VERIFIER_LENGTH {
                return (StatusCode::BAD_REQUEST, "code_verifier too long").into_response();
            }

            // Exchange the opaque authorization code
            // (handles client auth, PKCE verification, grant_type check internally)
            let response = match oauth
                .exchange_authorization_code(
                    &payload.code,
                    &payload.client_id,
                    &payload.client_secret,
                    &payload.redirect_uri,
                    &payload.code_verifier,
                    state.redis(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "authorization code exchange failed");
                    return (StatusCode::BAD_REQUEST, "Invalid authorization code").into_response();
                }
            };

            (headers, axum::Json(response)).into_response()
        }
        "refresh_token" => {
            // Verify the refresh token (must be token_type=refresh)
            let claims = match oauth.verify_refresh_token(&payload.refresh_token) {
                Ok(c) => c,
                Err(_) => {
                    return (StatusCode::BAD_REQUEST, "Invalid refresh token").into_response();
                }
            };

            // Authenticate client (must match the client that got the token)
            let client = match oauth
                .authenticate_client(&payload.client_id, &payload.client_secret)
                .await
            {
                Ok(c) => c,
                Err(_) => {
                    return (StatusCode::UNAUTHORIZED, "Client authentication failed")
                        .into_response();
                }
            };

            // Verify token was issued to this client
            if claims.client_id != client.client_id {
                return (
                    StatusCode::BAD_REQUEST,
                    "Token was not issued to this client",
                )
                    .into_response();
            }

            // Validate grant_type is allowed
            if !client.supports_grant_type("refresh_token") {
                return (
                    StatusCode::BAD_REQUEST,
                    "Grant type not allowed for this client",
                )
                    .into_response();
            }

            // Check revocation
            match oauth.is_revoked(&claims.jti, state.redis()).await {
                Ok(true) => {
                    return (StatusCode::BAD_REQUEST, "Refresh token has been revoked")
                        .into_response();
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed to check refresh token revocation");
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Unable to verify token status",
                    )
                        .into_response();
                }
            }

            let user_id: Uuid = match claims.sub.parse() {
                Ok(id) => id,
                Err(_) => {
                    return (StatusCode::BAD_REQUEST, "Invalid token subject").into_response();
                }
            };

            // Re-filter scope against current client restrictions.
            // A client's allowed scopes may have been reduced since the
            // original token was issued; the refreshed token should reflect that.
            let granted_scope = client.filter_scope(&claims.scope);

            let response =
                match oauth.create_access_token(user_id, &claims.client_id, &granted_scope, true) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "token creation failed");
                        return (StatusCode::INTERNAL_SERVER_ERROR, "Token creation failed")
                            .into_response();
                    }
                };

            // Revoke the old refresh token BEFORE returning the new one.
            // This is single-use enforcement per OAuth 2.1 best practice.
            // If revocation fails (e.g., Redis unavailable), we must deny the
            // request to prevent both old and new tokens from being valid.
            let now = chrono::Utc::now().timestamp();
            let remaining = (claims.exp - now).max(0) as u64;
            if let Err(e) = oauth
                .revoke_token(&claims.jti, remaining, state.redis())
                .await
            {
                tracing::warn!(error = %e, "failed to revoke old refresh token; denying request");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Unable to complete token rotation",
                )
                    .into_response();
            }

            (headers, axum::Json(response)).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, "Unsupported grant_type").into_response(),
    }
}

/// POST /oauth/revoke — revoke a token (RFC 7009).
///
/// Requires client authentication. The client must own the token being revoked.
async fn revoke(
    State(state): State<AppState>,
    axum::Form(payload): axum::Form<RevokeRequest>,
) -> impl IntoResponse {
    let Some(oauth) = state.oauth() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not enabled").into_response();
    };

    // Decode the token to get JTI and verify ownership
    let claims = match oauth.verify_token(&payload.token) {
        Ok(c) => c,
        Err(_) => {
            // Per RFC 7009 §2.2, return 200 even for invalid tokens
            return StatusCode::OK.into_response();
        }
    };

    // Verify client identity (RFC 7009 §2.1)
    if payload.client_id.is_empty() || claims.client_id != payload.client_id {
        // Per RFC 7009 §2.2, return 200 to avoid leaking token existence
        return StatusCode::OK.into_response();
    }

    // Authenticate confidential clients.
    // Fail-closed: if we can't look up the client (DB error), deny the request
    // rather than silently skipping authentication.
    match oauth.find_client(&payload.client_id).await {
        Ok(Some(client)) if client.is_confidential() => {
            let valid = oauth
                .validate_client_credentials(&payload.client_id, &payload.client_secret)
                .await
                .ok()
                .flatten();
            if valid.is_none() {
                return (StatusCode::UNAUTHORIZED, "Client authentication failed").into_response();
            }
        }
        Ok(Some(_)) => {} // Public client — no secret validation needed
        Ok(None) => {
            // Per RFC 7009 §2.2, return 200 for unknown clients to avoid leaking info
            return StatusCode::OK.into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to look up client for revocation");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    }

    // Calculate remaining TTL
    let now = chrono::Utc::now().timestamp();
    let remaining = (claims.exp - now).max(0) as u64;

    if let Err(e) = oauth
        .revoke_token(&claims.jti, remaining, state.redis())
        .await
    {
        tracing::warn!(error = %e, "failed to revoke token");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Revocation failed").into_response();
    }

    StatusCode::OK.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_validation_accepts_valid() {
        assert!(validate_scope(""));
        assert!(validate_scope("read"));
        assert!(validate_scope("read write"));
        assert!(validate_scope("openid profile email"));
    }

    #[test]
    fn scope_validation_rejects_invalid() {
        assert!(!validate_scope("scope\"injection"));
        assert!(!validate_scope("scope\\injection"));
        // Exceeds max length
        let long = "a".repeat(MAX_SCOPE_LENGTH + 1);
        assert!(!validate_scope(&long));
    }

    #[test]
    fn scope_at_max_length_accepted() {
        let exactly_max = "a".repeat(MAX_SCOPE_LENGTH);
        assert!(validate_scope(&exactly_max));
    }
}
