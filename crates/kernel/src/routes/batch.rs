//! Batch operations API.
//!
//! Provides REST endpoints for managing long-running batch operations
//! with progress polling support.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::batch::{BatchOperation, BatchStatus, CreateBatch};
use crate::error::AppError;
use crate::routes::helpers::require_csrf_header;
use crate::routes::item::get_user_context;
use crate::state::AppState;
use crate::tap::UserContext;

/// Verify the CSRF token carried in the `X-CSRF-Token` header, mapping a
/// failure onto an [`AppError`] 403. State-changing batch routes call this.
async fn verify_csrf(session: &Session, headers: &HeaderMap) -> Result<(), AppError> {
    require_csrf_header(session, headers).await.map_err(|_| {
        AppError::forbidden("Invalid or missing CSRF token. Include X-CSRF-Token header.")
    })
}

/// Load a batch operation and enforce that `user` may act on it.
///
/// A batch is visible only to its owner or an admin. A non-owner gets **404**
/// (not 403) so batch IDs cannot be probed for existence — this closes the
/// IDOR half of AC-W2/CSRF-2. Legacy rows (`owner_id == nil`) are admin-only.
async fn load_owned_batch(
    state: &AppState,
    user: &UserContext,
    id: Uuid,
) -> Result<BatchOperation, AppError> {
    let op = state
        .batch()
        .get(id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "get batch operation"))?
        .ok_or_else(|| AppError::not_found_id("batch operation", id))?;

    if op.owner_id != user.id && !user.is_admin() {
        // Do not leak that the id exists to a non-owner.
        return Err(AppError::not_found_id("batch operation", id));
    }
    Ok(op)
}

/// Response for batch operation creation.
#[derive(Serialize)]
struct CreateBatchResponse {
    id: Uuid,
    status: BatchStatus,
}

/// Response for batch operation status.
#[derive(Serialize)]
struct BatchStatusResponse {
    id: Uuid,
    operation_type: String,
    status: BatchStatus,
    progress: BatchProgressResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    created: i64,
    updated: i64,
}

/// Progress information in response.
#[derive(Serialize)]
struct BatchProgressResponse {
    total: u64,
    processed: u64,
    percentage: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_operation: Option<String>,
}

/// Create a new batch operation.
///
/// POST /api/batch
async fn create_batch(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(input): Json<CreateBatch>,
) -> Result<(StatusCode, Json<CreateBatchResponse>), AppError> {
    // Authentication + CSRF close the AC-W2 Critical (create was anonymous).
    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return Err(AppError::unauthorized("authentication required"));
    }
    verify_csrf(&session, &headers).await?;

    let operation = state
        .batch()
        .create(input, user.id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "create batch operation"))?;

    Ok((
        StatusCode::CREATED,
        Json(CreateBatchResponse {
            id: operation.id,
            status: operation.status,
        }),
    ))
}

/// Get batch operation status.
///
/// GET /api/batch/{id}
async fn get_batch(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Result<Json<BatchStatusResponse>, AppError> {
    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return Err(AppError::unauthorized("authentication required"));
    }
    let operation = load_owned_batch(&state, &user, id).await?;

    Ok(Json(operation_to_response(operation)))
}

/// Cancel a batch operation.
///
/// POST /api/batch/{id}/cancel
async fn cancel_batch(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<BatchStatusResponse>, AppError> {
    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return Err(AppError::unauthorized("authentication required"));
    }
    verify_csrf(&session, &headers).await?;
    // Ownership gate before the state change (404 for non-owners).
    load_owned_batch(&state, &user, id).await?;

    state.batch().cancel(id).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            AppError::not_found_id("batch operation", id)
        } else if msg.contains("cannot cancel") {
            AppError::conflict(msg)
        } else {
            AppError::internal_ctx(e, "cancel batch operation")
        }
    })?;

    // Fetch updated operation
    let operation = state
        .batch()
        .get(id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "get batch operation after cancel"))?
        .ok_or_else(|| AppError::not_found_id("batch operation", id))?;

    Ok(Json(operation_to_response(operation)))
}

/// Delete a batch operation.
///
/// DELETE /api/batch/{id}
async fn delete_batch(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let user = get_user_context(&session, &state).await;
    if !user.authenticated {
        return Err(AppError::unauthorized("authentication required"));
    }
    verify_csrf(&session, &headers).await?;
    // Ownership gate before the delete (404 for non-owners).
    load_owned_batch(&state, &user, id).await?;

    let deleted = state
        .batch()
        .delete(id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "delete batch operation"))?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found_id("batch operation", id))
    }
}

/// Convert BatchOperation to response format.
fn operation_to_response(op: BatchOperation) -> BatchStatusResponse {
    BatchStatusResponse {
        id: op.id,
        operation_type: op.operation_type,
        status: op.status,
        progress: BatchProgressResponse {
            total: op.progress.total,
            processed: op.progress.processed,
            percentage: op.progress.percentage,
            current_operation: op.progress.current_operation,
        },
        result: op.result,
        error: op.error,
        created: op.created,
        updated: op.updated,
    }
}

/// Create the batch operations router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/batch", post(create_batch))
        .route("/api/batch/{id}", get(get_batch))
        .route("/api/batch/{id}/cancel", post(cancel_batch))
        .route("/api/batch/{id}", delete(delete_batch))
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_to_response() {
        let op = BatchOperation {
            id: Uuid::nil(),
            owner_id: Uuid::nil(),
            operation_type: "test".to_string(),
            status: BatchStatus::Pending,
            progress: crate::batch::BatchProgress::default(),
            params: serde_json::Value::Null,
            result: None,
            error: None,
            created: 1000,
            updated: 1000,
        };

        let response = operation_to_response(op);
        assert_eq!(response.operation_type, "test");
        assert_eq!(response.progress.percentage, 0);
    }
}
