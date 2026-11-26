//! The dead jobs report: `/admin/reports/dead-jobs` (B252).
//!
//! Plugin-queue jobs the v2 drain gave up on after `max_attempts` failures,
//! with their reason, so an administrator can see, retry or discard them
//! without reaching for the JSON API or `psql`. Pages through the admin
//! listing pattern ([`super::admin_list`]): filtered by queue and plugin,
//! sortable, 50 a page. Retry and Discard post to the same
//! [`super::admin_queue::requeue_dead`] and [`super::admin_queue::discard_dead`]
//! the JSON API calls, so there is one query path for both surfaces. Discard
//! asks first, through the shared confirmation layer
//! ([`super::admin_confirm`]); Retry does not, since it only returns a job to
//! the queue it was already in.
//!
//! Takes `administer site`, the same permission the JSON API requires.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::state::AppState;

use super::admin_list::{ListQuery, PER_PAGE};
use super::admin_queue::{DeadJobFilter, PERMISSION, discard_dead, list_dead, requeue_dead};
use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_csrf,
    require_permission,
};

/// This page's address.
pub const PATH: &str = "/admin/reports/dead-jobs";

/// Session key for the flash message after Retry or Discard.
const FLASH_KEY: &str = "dead_jobs_flash";

/// The listing's sortable columns: key and SQL.
const DLQ_SORTS: &[(&str, &str)] = &[
    ("dead_at", "dead_at"),
    ("queue", "queue_name"),
    ("plugin", "plugin_name"),
    ("attempts", "attempts"),
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(PATH, get(dead_jobs_page))
        .route(
            "/admin/reports/dead-jobs/{id}/retry",
            axum::routing::post(retry_job),
        )
        .route(
            "/admin/reports/dead-jobs/{id}/discard",
            axum::routing::post(discard_job),
        )
}

/// The `ORDER BY` clause for a sort: the default keeps the API's own tie
/// break (newest death first, undated rows last); any other column breaks
/// ties by id, as every other listing does.
fn order_by(sort: &super::admin_list::Sort<'_>) -> String {
    if sort.key == "dead_at" && sort.desc {
        "dead_at DESC NULLS LAST, id DESC".to_string()
    } else {
        format!("{}, id DESC", sort.order_by())
    }
}

/// The distinct values of `column` among dead jobs, for a filter's options.
async fn distinct_values(state: &AppState, column: &str) -> Vec<String> {
    // `column` is one of two literals this function is called with, never
    // request input.
    let sql = format!(
        "SELECT DISTINCT {column} FROM plugin_queue WHERE status = 'dead' ORDER BY {column}"
    );
    sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql))
        .fetch_all(state.db())
        .await
        .unwrap_or_default()
}

/// GET /admin/reports/dead-jobs
async fn dead_jobs_page(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let query = ListQuery::from_params(&params);
    let sort = query.sort(DLQ_SORTS, ("dead_at", true));
    let order_by = order_by(&sort);

    let queue_filter = params
        .get("queue")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let plugin_filter = params
        .get("plugin")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let filter = DeadJobFilter {
        queue_name: queue_filter.clone(),
        plugin_name: plugin_filter.clone(),
    };

    let (jobs, total) = match list_dead(&state, &filter, &order_by, PER_PAGE, query.offset()).await
    {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the dead jobs report");
            return render_server_error("The dead jobs report could not be read.");
        }
    };

    let queues = distinct_values(&state, "queue_name").await;
    let plugins = distinct_values(&state, "plugin_name").await;
    let queue_options: Vec<(&str, &str)> =
        queues.iter().map(|q| (q.as_str(), q.as_str())).collect();
    let plugin_options: Vec<(&str, &str)> =
        plugins.iter().map(|p| (p.as_str(), p.as_str())).collect();

    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("list", &query.view(total, jobs.len(), Some(&sort)));
    context.insert("jobs", &jobs);
    context.insert("queue_options", &queue_options);
    context.insert("plugin_options", &plugin_options);
    context.insert("queue_filter", &queue_filter.unwrap_or_default());
    context.insert("plugin_filter", &plugin_filter.unwrap_or_default());
    context.insert("flash", &flash);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", PATH);
    render_admin_template(&state, &viewer, "admin/reports/dead-jobs.html", context).await
}

/// POST /admin/reports/dead-jobs/{id}/retry
async fn retry_job(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(refusal) = require_permission(&state, &session, PERMISSION).await {
        return refusal;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    match requeue_dead(&state, id).await {
        Ok(true) => {
            tracing::info!(item_id = id, "dead job requeued from the admin page");
            let _ = session
                .insert(FLASH_KEY, "The job was requeued.".to_string())
                .await;
            Redirect::to(PATH).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to requeue a dead job");
            render_server_error("The job could not be requeued.")
        }
    }
}

/// POST /admin/reports/dead-jobs/{id}/discard
async fn discard_job(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(refusal) = require_permission(&state, &session, PERMISSION).await {
        return refusal;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    match discard_dead(&state, id).await {
        Ok(true) => {
            tracing::info!(item_id = id, "dead job discarded from the admin page");
            let _ = session
                .insert(FLASH_KEY, "The job was discarded.".to_string())
                .await;
            Redirect::to(PATH).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to discard a dead job");
            render_server_error("The job could not be discarded.")
        }
    }
}
