//! Admin surface for the plugin-queue dead-letter tier (P11d / D-46).
//!
//! The v2 drain moves a job to `status = 'dead'` once it fails `max_attempts`
//! times, so a poison item never blocks its queue or retries forever. These
//! JSON endpoints, under `/api/v1` with every other kernel JSON route, make the
//! DLQ inspectable and recoverable:
//!
//! - `GET  /api/v1/queue/dlq` — a page of dead-lettered jobs with their reason;
//! - `POST /api/v1/queue/dlq/{id}/requeue` — reset a dead job to `ready` for
//!   another run (attempts cleared);
//! - `POST /api/v1/queue/dlq/{id}/delete` — discard a dead job.
//!
//! Every one requires `administer site`; mutations also a valid
//! `X-CSRF-Token` header.
//! A pure SQL/CLI view of the same data is documented in
//! `docs/plugin-queue.md` for operators who prefer psql.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde_json::json;
use sqlx::Row;
use tower_sessions::Session;

use axum::extract::Query;

use crate::error::AppError;
use crate::pagination::{Page, PageParams};
use crate::state::AppState;

use super::helpers::{require_csrf_header, require_permission_json};

/// The permission the dead-letter endpoints require.
const PERMISSION: &str = "administer site";

/// The dead-letter JSON API, under `/api/v1` ([`super::api`]).
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("queue")
        .get(
            "/queue/dlq",
            list_dlq,
            doc("List dead-lettered plugin queue jobs, newest first (`administer site`)")
                .paged(50, 500),
        )
        .post(
            "/queue/dlq/{id}/requeue",
            requeue_dlq,
            doc("Requeue a dead-lettered job (`administer site`; `X-CSRF-Token`)"),
        )
        .post(
            "/queue/dlq/{id}/delete",
            delete_dlq,
            doc("Delete a dead-lettered job (`administer site`; `X-CSRF-Token`)"),
        )
}

/// List dead-lettered jobs (newest first).
async fn list_dlq(
    State(state): State<AppState>,
    session: Session,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<serde_json::Value>>, AppError> {
    require_permission_json(&state, &session, PERMISSION).await?;
    let window = paging.window(50, 500);

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plugin_queue WHERE status = 'dead'")
        .fetch_one(state.db())
        .await
        .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "count dead-letter queue"))?;

    let rows = sqlx::query(
        r#"
        SELECT id, plugin_name, queue_name, attempts, max_attempts,
               dead_reason, dead_at, last_error, created_at
        FROM plugin_queue
        WHERE status = 'dead'
        ORDER BY dead_at DESC NULLS LAST, id DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(window.per_page)
    .bind(window.offset)
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

    Ok(Json(Page::new(
        jobs,
        u64::try_from(total).unwrap_or(0),
        window,
    )))
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
    require_permission_json(&state, &session, PERMISSION).await?;

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
    require_permission_json(&state, &session, PERMISSION).await?;

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
