//! Admin surface for the plugin-queue dead-letter tier (P11d / D-46, B252).
//!
//! The v2 drain moves a job to `status = 'dead'` once it fails `max_attempts`
//! times, so a poison item never blocks its queue or retries forever. The data
//! is reachable two ways, both calling the functions below so there is one
//! query path for listing, requeuing and deleting a dead job:
//!
//! - JSON, under `/api/v1` with every other kernel JSON route:
//!   `GET /api/v1/queue/dlq`, `POST /api/v1/queue/dlq/{id}/requeue`,
//!   `POST /api/v1/queue/dlq/{id}/delete`. Every one requires
//!   `administer site`; mutations also a valid `X-CSRF-Token` header.
//! - HTML, at `/admin/reports/dead-jobs` ([`super::admin_dead_jobs`]), for an
//!   administrator who would rather not reach for `curl`.
//!
//! A pure SQL/CLI view of the same data is documented in
//! `docs/plugin-queue.md` for operators who prefer psql.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::Serialize;
use serde_json::json;
use sqlx::AssertSqlSafe;
use tower_sessions::Session;

use axum::extract::Query;

use crate::error::AppError;
use crate::pagination::{Page, PageParams};
use crate::state::AppState;

use super::helpers::{require_csrf_header, require_permission_json};

/// The permission the dead-letter endpoints require.
pub(crate) const PERMISSION: &str = "administer site";

/// One dead-lettered job, as the JSON API and the admin page both show it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub(crate) struct DeadJob {
    pub id: i64,
    pub plugin_name: String,
    pub queue_name: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub dead_reason: Option<String>,
    pub dead_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
}

/// The dead jobs listing's exact-match filters: by queue, then by plugin.
#[derive(Debug, Default, Clone)]
pub(crate) struct DeadJobFilter {
    pub queue_name: Option<String>,
    pub plugin_name: Option<String>,
}

/// A page of dead-lettered jobs matching `filter`, in `order_by` order (a
/// trusted SQL fragment; never request input), with the total that match.
pub(crate) async fn list_dead(
    state: &AppState,
    filter: &DeadJobFilter,
    order_by: &str,
    limit: i64,
    offset: i64,
) -> Result<(Vec<DeadJob>, i64), sqlx::Error> {
    let mut clauses = vec!["status = 'dead'".to_string()];
    let mut n = 0usize;
    if filter.queue_name.is_some() {
        n += 1;
        clauses.push(format!("queue_name = ${n}"));
    }
    if filter.plugin_name.is_some() {
        n += 1;
        clauses.push(format!("plugin_name = ${n}"));
    }
    let where_sql = format!(" WHERE {}", clauses.join(" AND "));
    let count_sql = format!("SELECT COUNT(*) FROM plugin_queue{where_sql}");
    let list_sql = format!(
        "SELECT id, plugin_name, queue_name, attempts, max_attempts, \
                dead_reason, dead_at, last_error, created_at \
         FROM plugin_queue{where_sql} ORDER BY {order_by} LIMIT ${} OFFSET ${}",
        n + 1,
        n + 2
    );
    let mut count_q = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql));
    let mut list_q = sqlx::query_as::<_, DeadJob>(AssertSqlSafe(list_sql));
    if let Some(q) = &filter.queue_name {
        count_q = count_q.bind(q);
        list_q = list_q.bind(q);
    }
    if let Some(p) = &filter.plugin_name {
        count_q = count_q.bind(p);
        list_q = list_q.bind(p);
    }
    let total = count_q.fetch_one(state.db()).await?;
    let rows = list_q
        .bind(limit)
        .bind(offset)
        .fetch_all(state.db())
        .await?;
    Ok((rows, total))
}

/// Requeue a dead-lettered job: reset it to `ready` with a fresh attempt
/// budget. `true` when a dead job with this id existed.
pub(crate) async fn requeue_dead(state: &AppState, id: i64) -> Result<bool, sqlx::Error> {
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
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// Discard a dead-lettered job. `true` when a dead job with this id existed.
pub(crate) async fn discard_dead(state: &AppState, id: i64) -> Result<bool, sqlx::Error> {
    let affected = sqlx::query("DELETE FROM plugin_queue WHERE id = $1 AND status = 'dead'")
        .bind(id)
        .execute(state.db())
        .await?
        .rows_affected();
    Ok(affected > 0)
}

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
) -> Result<Json<Page<DeadJob>>, AppError> {
    require_permission_json(&state, &session, PERMISSION).await?;
    let window = paging.window(50, 500);

    let (jobs, total) = list_dead(
        &state,
        &DeadJobFilter::default(),
        "dead_at DESC NULLS LAST, id DESC",
        window.per_page,
        window.offset,
    )
    .await
    .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "list dead-letter queue"))?;

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

    let found = requeue_dead(&state, id)
        .await
        .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "requeue dead-letter job"))?;
    if !found {
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

    let found = discard_dead(&state, id)
        .await
        .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "delete dead-letter job"))?;
    if !found {
        return Err(AppError::not_found("No dead-lettered job with that id"));
    }
    tracing::info!(item_id = id, "dead-letter job deleted");
    Ok(Json(json!({ "success": true, "deleted": id })))
}
