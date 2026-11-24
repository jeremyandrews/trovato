//! Content lock routes.
//!
//! Provides API endpoints for content lock heartbeat and break operations.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;

use crate::error::AppError;
use crate::state::AppState;

/// Lock request payload.
#[derive(Debug, Deserialize)]
pub struct LockRequest {
    pub entity_type: String,
    pub entity_id: String,
}

/// Allowed entity types for content locking.
///
/// Prevents arbitrary strings from being used as entity types, which could
/// pollute the editing_lock table or be used for denial-of-service.
const ALLOWED_ENTITY_TYPES: &[&str] = &["item", "category", "comment", "media"];

/// Maximum length for entity_id to prevent storage abuse.
const MAX_ENTITY_ID_LENGTH: usize = 200;

/// The content lock JSON API, under `/api/v1` ([`super::api`]), served while
/// `trovato_content_locking` is enabled.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("locking")
        .post(
            "/lock/heartbeat",
            heartbeat,
            doc("Extend the caller's edit lock (`X-CSRF-Token`)").alias(),
        )
        .post(
            "/lock/break",
            break_lock,
            doc("Break another user's edit lock (`break content lock`; `X-CSRF-Token`)").alias(),
        )
        .post(
            "/lock/release",
            release,
            doc(
                "Release the caller's own edit lock at once, e.g. on `pagehide` via \
                 `navigator.sendBeacon` (`X-CSRF-Token`, or a form-urlencoded `_token` field)",
            )
            .alias(),
        )
        .plugin("trovato_content_locking")
}

/// The answer to a heartbeat or a break: what happened, and a fresh CSRF
/// token for the next request. Tokens are single use, and the edit form beats
/// every minute for as long as it is open, so each answer carries the next
/// one.
async fn answer(session: &tower_sessions::Session, lock: &str) -> axum::response::Response {
    let csrf_token = crate::form::csrf::generate_csrf_token(session).await;
    axum::Json(serde_json::json!({ "lock": lock, "csrf_token": csrf_token })).into_response()
}

/// POST /api/v1/lock/heartbeat — extend lock expiration.
async fn heartbeat(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    headers: HeaderMap,
    axum::Json(payload): axum::Json<LockRequest>,
) -> impl IntoResponse {
    if !ALLOWED_ENTITY_TYPES.contains(&payload.entity_type.as_str()) {
        return AppError::with_status(StatusCode::BAD_REQUEST, "Invalid entity type")
            .into_response();
    }
    if payload.entity_id.len() > MAX_ENTITY_ID_LENGTH {
        return AppError::with_status(StatusCode::BAD_REQUEST, "entity_id too long")
            .into_response();
    }

    let user_id = match crate::routes::helpers::require_login_json(&state, &session).await {
        Ok(user) => user.id,
        Err(refusal) => return refusal.into_response(),
    };

    // Verify CSRF token from header
    if let Err(refusal) = crate::routes::helpers::require_csrf_header(&session, &headers).await {
        return refusal.into_response();
    }

    let Some(lock_service) = state.content_lock() else {
        return AppError::with_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "Content locking not enabled",
        )
        .into_response();
    };

    match lock_service
        .heartbeat(&payload.entity_type, &payload.entity_id, user_id)
        .await
    {
        Ok(true) => answer(&session, "extended").await,
        Ok(false) => {
            AppError::with_status(StatusCode::CONFLICT, "Lock not held by you").into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "lock heartbeat failed");
            AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Lock heartbeat failed")
                .into_response()
        }
    }
}

/// POST /api/v1/lock/break — break a lock (requires permission).
async fn break_lock(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    headers: HeaderMap,
    axum::Json(payload): axum::Json<LockRequest>,
) -> impl IntoResponse {
    if !ALLOWED_ENTITY_TYPES.contains(&payload.entity_type.as_str()) {
        return AppError::with_status(StatusCode::BAD_REQUEST, "Invalid entity type")
            .into_response();
    }
    if payload.entity_id.len() > MAX_ENTITY_ID_LENGTH {
        return AppError::with_status(StatusCode::BAD_REQUEST, "entity_id too long")
            .into_response();
    }

    if let Err(refusal) =
        crate::routes::helpers::require_permission_json(&state, &session, "break content lock")
            .await
    {
        return refusal.into_response();
    }

    // Verify CSRF token from header
    if let Err(refusal) = crate::routes::helpers::require_csrf_header(&session, &headers).await {
        return refusal.into_response();
    }

    let Some(lock_service) = state.content_lock() else {
        return AppError::with_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "Content locking not enabled",
        )
        .into_response();
    };

    match lock_service
        .break_lock(&payload.entity_type, &payload.entity_id)
        .await
    {
        Ok(true) => answer(&session, "broken").await,
        Ok(false) => AppError::with_status(StatusCode::NOT_FOUND, "No lock found").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "break lock failed");
            AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Break lock failed")
                .into_response()
        }
    }
}

/// Whether the request body is a URL-encoded form (ignoring a `charset` parameter).
fn is_form_urlencoded(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
        .unwrap_or(false)
}

/// The release payload, read from JSON like heartbeat and break, or — because
/// `navigator.sendBeacon` cannot set a custom header, which is how the other
/// two routes carry a CSRF token — from a form-urlencoded body, the same shape
/// [`super::helpers::require_csrf_header_or_field`] already accepts a token
/// from.
fn parse_release_payload(headers: &HeaderMap, body: &str) -> Option<LockRequest> {
    if is_form_urlencoded(headers) {
        Some(LockRequest {
            entity_type: super::helpers::single_form_field(body, "entity_type")?,
            entity_id: super::helpers::single_form_field(body, "entity_id")?,
        })
    } else {
        serde_json::from_str(body).ok()
    }
}

/// POST /api/v1/lock/release — release the caller's own edit lock at once.
///
/// This is the route `content-lock.js` beacons on `pagehide`, so that a closed
/// tab or an away navigation frees the item immediately rather than waiting
/// out the lease. A save already releases the lock on success
/// ([`crate::routes::item::release_edit_lock`]); a release that finds nothing
/// held is not an error, since the beacon fires whether or not that already
/// happened.
async fn release(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let body = String::from_utf8_lossy(&body).into_owned();
    let Some(payload) = parse_release_payload(&headers, &body) else {
        return AppError::with_status(StatusCode::BAD_REQUEST, "Invalid request body")
            .into_response();
    };

    if !ALLOWED_ENTITY_TYPES.contains(&payload.entity_type.as_str()) {
        return AppError::with_status(StatusCode::BAD_REQUEST, "Invalid entity type")
            .into_response();
    }
    if payload.entity_id.len() > MAX_ENTITY_ID_LENGTH {
        return AppError::with_status(StatusCode::BAD_REQUEST, "entity_id too long")
            .into_response();
    }

    let user_id = match crate::routes::helpers::require_login_json(&state, &session).await {
        Ok(user) => user.id,
        Err(refusal) => return refusal.into_response(),
    };

    if let Err(refusal) =
        crate::routes::helpers::require_csrf_header_or_field(&session, &headers, &body).await
    {
        return refusal.into_response();
    }

    let Some(lock_service) = state.content_lock() else {
        return AppError::with_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "Content locking not enabled",
        )
        .into_response();
    };

    match lock_service
        .release(&payload.entity_type, &payload.entity_id, user_id)
        .await
    {
        Ok(_) => answer(&session, "released").await,
        Err(e) => {
            tracing::warn!(error = %e, "lock release failed");
            AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Lock release failed")
                .into_response()
        }
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_validation() {
        for valid in &["item", "category", "comment", "media"] {
            assert!(
                ALLOWED_ENTITY_TYPES.contains(valid),
                "{valid} should be allowed"
            );
        }
        for invalid in &["", "admin", "../../etc", "user", "anything_else"] {
            assert!(
                !ALLOWED_ENTITY_TYPES.contains(invalid),
                "{invalid} should be rejected"
            );
        }
    }
}
