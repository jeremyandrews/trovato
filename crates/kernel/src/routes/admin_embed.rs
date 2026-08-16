//! Admin surface for the async auto-embed lifecycle (P11f / D-51, D-52).
//!
//! Item embeddings are generated asynchronously off the save path (D-52). These
//! admin-only JSON endpoints make that lifecycle observable and operable:
//!
//! - `GET  /admin/embed/status` — counts of items by embedding state
//!   (`pending` / `indexed` / `failed`, D-51's observable state);
//! - `POST /admin/embed/backfill` — enqueue embed jobs for items missing an
//!   embedding for the active model, in a bounded batch. This is also the
//!   **model-change re-embed** trigger: after the embedding model changes,
//!   every item is "missing an embedding for the active model", so a backfill
//!   re-enqueues them (old-model vectors are harmlessly retained — the
//!   similarity read path filters by the active model).
//!
//! Mutations require a valid `X-CSRF-Token` header and admin authorization.

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use tower_sessions::Session;

use crate::error::AppError;
use crate::services::embed_index;
use crate::state::AppState;

use super::helpers::{require_admin_json, require_csrf_header};

/// Default backfill batch size when `limit` is not supplied.
const DEFAULT_BACKFILL_LIMIT: i64 = 500;

/// Kernel ceiling on a single backfill batch, so one admin request can never
/// enqueue an unbounded number of jobs.
const MAX_BACKFILL_LIMIT: i64 = 5000;

/// Embedding-state counts, admin only (D-51 observable state).
async fn embed_status(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin_json(&state, &session).await?;

    let counts = embed_index::status_counts(state.db())
        .await
        .map_err(|e| AppError::internal_ctx(e, "aggregate embed status"))?;

    Ok(Json(json!({
        "pending": counts.pending,
        "indexed": counts.indexed,
        "failed": counts.failed,
    })))
}

/// Backfill query parameters.
#[derive(Debug, Deserialize)]
struct BackfillParams {
    /// Maximum items to enqueue this batch (clamped to `MAX_BACKFILL_LIMIT`).
    limit: Option<i64>,
}

/// Enqueue a bounded batch of embed jobs for items missing an embedding for the
/// active model (P11f backfill / model-change re-embed), admin only.
async fn backfill(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Query(params): Query<BackfillParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;
    require_admin_json(&state, &session).await?;

    let limit = params
        .limit
        .unwrap_or(DEFAULT_BACKFILL_LIMIT)
        .clamp(1, MAX_BACKFILL_LIMIT);

    let Some(model) = state.ai_providers().embedding_model().await else {
        return Err(AppError::bad_request(
            "No embedding provider is configured; nothing to backfill",
        ));
    };

    let enqueued = embed_index::enqueue_backfill(state.db(), &model, limit)
        .await
        .map_err(|e| AppError::internal_ctx(e, "enqueue embed backfill"))?;

    tracing::info!(model = %model, enqueued, limit, "embed backfill enqueued");
    Ok(Json(json!({
        "success": true,
        "model": model,
        "enqueued": enqueued,
        "limit": limit,
    })))
}

/// Router for the async-embed admin surface.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/embed/status", get(embed_status))
        .route("/admin/embed/backfill", post(backfill))
}
