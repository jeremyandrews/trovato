//! Admin surface for the plugin-queue dead-letter tier (P11d / D-46).
//!
//! The v2 drain moves a job to `status = 'dead'` once it fails `max_attempts`
//! times, so a poison item never blocks its queue or retries forever. These
//! admin-only JSON endpoints make the DLQ inspectable and recoverable:
//!
//! - `GET  /admin/queue/dlq` — list dead-lettered jobs with their reason;
//! - `POST /admin/queue/dlq/{id}/requeue` — reset a dead job to `ready` for
//!   another run (attempts cleared);
//! - `POST /admin/queue/dlq/{id}/delete` — discard a dead job.
//!
//! Mutations require a valid `X-CSRF-Token` header and admin authorization.
//! A pure SQL/CLI view of the same data is documented in
//! `docs/plugin-queue.md` for operators who prefer psql.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde_json::json;
use sqlx::Row;
use tower_sessions::Session;

use axum::Router;
use axum::routing::{get, post};

use crate::error::AppError;
use crate::state::AppState;

use super::helpers::{require_admin_json, require_csrf_header};

/// Maximum dead-letter rows returned by the list endpoint.
const DLQ_LIST_LIMIT: i64 = 500;

/// List dead-lettered jobs (newest first), admin only.
async fn list_dlq(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin_json(&state, &session).await?;

    let rows = sqlx::query(
        r#"
        SELECT id, plugin_name, queue_name, attempts, max_attempts,
               dead_reason, dead_at, last_error, created_at
        FROM plugin_queue
        WHERE status = 'dead'
        ORDER BY dead_at DESC NULLS LAST, id DESC
        LIMIT $1
        "#,
    )
    .bind(DLQ_LIST_LIMIT)
    .fetch_all(state.db())
    .await
    .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "list dead-letter queue"))?;

    let jobs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<i64, _>("id"),
                "plugin_name": row.get::<String, _>("plugin_name"),
                "queue_name": row.get::<String, _>("queue_name"),
                "attempts": row.get::<i32, _>("attempts"),
                "max_attempts": row.get::<i32, _>("max_attempts"),
                "dead_reason": row.get::<Option<String>, _>("dead_reason"),
                "dead_at": row.get::<Option<i64>, _>("dead_at"),
                "last_error": row.get::<Option<String>, _>("last_error"),
                "created_at": row.get::<i64, _>("created_at"),
            })
        })
        .collect();

    Ok(Json(json!({ "count": jobs.len(), "jobs": jobs })))
}

/// Requeue a dead-lettered job: reset it to `ready` with a fresh attempt budget.
async fn requeue_dlq(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;
    require_admin_json(&state, &session).await?;

    // Only affects a row that is actually dead — a no-op otherwise.
    let affected = sqlx::query(
        r#"
        UPDATE plugin_queue
        SET status = 'ready', attempts = 0, next_attempt_at = 0, locked_until = 0,
            dead_reason = NULL, dead_at = NULL, last_error = NULL
        WHERE id = $1 AND status = 'dead'
        "#,
    )
    .bind(id)
    .execute(state.db())
    .await
    .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "requeue dead-letter job"))?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::not_found("No dead-lettered job with that id"));
    }
    tracing::info!(item_id = id, "dead-letter job requeued");
    Ok(Json(json!({ "success": true, "requeued": id })))
}

/// Delete a dead-lettered job.
async fn delete_dlq(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;
    require_admin_json(&state, &session).await?;

    let affected = sqlx::query("DELETE FROM plugin_queue WHERE id = $1 AND status = 'dead'")
        .bind(id)
        .execute(state.db())
        .await
        .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "delete dead-letter job"))?
        .rows_affected();

    if affected == 0 {
        return Err(AppError::not_found("No dead-lettered job with that id"));
    }
    tracing::info!(item_id = id, "dead-letter job deleted");
    Ok(Json(json!({ "success": true, "deleted": id })))
}

/// Router for the DLQ admin surface.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/queue/dlq", get(list_dlq))
        .route("/admin/queue/dlq/{id}/requeue", post(requeue_dlq))
        .route("/admin/queue/dlq/{id}/delete", post(delete_dlq))
}
