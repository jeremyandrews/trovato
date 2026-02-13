//! Batch operations API.
//!
//! Provides REST endpoints for managing long-running batch operations
//! with progress polling support.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::batch::{BatchOperation, BatchStatus, CreateBatch};
use crate::state::AppState;

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

/// Error response.
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Create a new batch operation.
///
/// POST /api/batch
async fn create_batch(
    State(state): State<AppState>,
    Json(input): Json<CreateBatch>,
) -> Result<(StatusCode, Json<CreateBatchResponse>), (StatusCode, Json<ErrorResponse>)> {
    let operation = state.batch().create(input).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

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
    Path(id): Path<Uuid>,
) -> Result<Json<BatchStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let operation = state
        .batch()
        .get(id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Batch operation not found".to_string(),
                }),
            )
        })?;

    Ok(Json(operation_to_response(operation)))
}

/// Cancel a batch operation.
///
/// POST /api/batch/{id}/cancel
async fn cancel_batch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<BatchStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    state.batch().cancel(id).await.map_err(|e| {
        let status = if e.to_string().contains("not found") {
            StatusCode::NOT_FOUND
        } else if e.to_string().contains("cannot cancel") {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (
            status,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    // Fetch updated operation
    let operation = state
        .batch()
        .get(id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Batch operation not found".to_string(),
                }),
            )
        })?;

    Ok(Json(operation_to_response(operation)))
}

/// Delete a batch operation.
///
/// DELETE /api/batch/{id}
async fn delete_batch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let deleted = state.batch().delete(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Batch operation not found".to_string(),
            }),
        ))
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
mod tests {
    use super::*;

    #[test]
    fn test_operation_to_response() {
        let op = BatchOperation {
            id: Uuid::nil(),
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
