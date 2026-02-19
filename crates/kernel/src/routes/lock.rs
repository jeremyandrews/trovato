//! Content lock routes.
//!
//! Provides API endpoints for content lock heartbeat and break operations.

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use serde::Deserialize;
use uuid::Uuid;

use crate::models::User;
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

/// Create the lock routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/lock/heartbeat", post(heartbeat))
        .route("/api/lock/break", post(break_lock))
}

/// POST /api/lock/heartbeat — extend lock expiration.
async fn heartbeat(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    axum::Json(payload): axum::Json<LockRequest>,
) -> impl IntoResponse {
    if !ALLOWED_ENTITY_TYPES.contains(&payload.entity_type.as_str()) {
        return (StatusCode::BAD_REQUEST, "Invalid entity type").into_response();
    }
    if payload.entity_id.len() > MAX_ENTITY_ID_LENGTH {
        return (StatusCode::BAD_REQUEST, "entity_id too long").into_response();
    }

    let Some(user_id) = session.get::<Uuid>("user_id").await.ok().flatten() else {
        return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response();
    };

    let Some(lock_service) = state.content_lock() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Content locking not enabled",
        )
            .into_response();
    };

    match lock_service
        .heartbeat(&payload.entity_type, &payload.entity_id, user_id)
        .await
    {
        Ok(true) => (StatusCode::OK, "Lock extended").into_response(),
        Ok(false) => (StatusCode::CONFLICT, "Lock not held by you").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "lock heartbeat failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Lock heartbeat failed").into_response()
        }
    }
}

/// POST /api/lock/break — break a lock (requires permission).
async fn break_lock(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    axum::Json(payload): axum::Json<LockRequest>,
) -> impl IntoResponse {
    if !ALLOWED_ENTITY_TYPES.contains(&payload.entity_type.as_str()) {
        return (StatusCode::BAD_REQUEST, "Invalid entity type").into_response();
    }
    if payload.entity_id.len() > MAX_ENTITY_ID_LENGTH {
        return (StatusCode::BAD_REQUEST, "entity_id too long").into_response();
    }

    let Some(user_id) = session.get::<Uuid>("user_id").await.ok().flatten() else {
        return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response();
    };

    // Check permission - load user to check against their roles
    let user = match User::find_by_id(state.db(), user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return (StatusCode::UNAUTHORIZED, "User not found").into_response(),
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load user").into_response();
        }
    };

    let has_perm = state
        .permissions()
        .user_has_permission(&user, "break content lock")
        .await
        .unwrap_or(false);

    if !has_perm {
        return (
            StatusCode::FORBIDDEN,
            "Missing 'break content lock' permission",
        )
            .into_response();
    }

    let Some(lock_service) = state.content_lock() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Content locking not enabled",
        )
            .into_response();
    };

    match lock_service
        .break_lock(&payload.entity_type, &payload.entity_id)
        .await
    {
        Ok(true) => (StatusCode::OK, "Lock broken").into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "No lock found").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "break lock failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Break lock failed").into_response()
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
