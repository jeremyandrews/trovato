//! The request profiler's pages.
//!
//! `GET /admin/reports/requests`: the recent requests the profiler kept, newest
//! first. `GET /admin/reports/requests/{id}`: one of them, with its phase tree,
//! its statements, its tap calls and its cache hits and misses.
//!
//! Both answer 404 to everyone while `PROFILER_ENABLED` is off, before any
//! permission is asked for: off, the pages do not exist. On, they ask for
//! `view request profiles`, which no role holds by default (the superuser
//! holds every permission). Requests to them are not themselves profiled.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use tower_sessions::Session;

use crate::profiler::{PAGES, PERMISSION};
use crate::state::AppState;

use super::helpers::{
    get_user_context, render_admin_template, render_not_found, require_permission,
};

/// Build the router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(PAGES, get(list))
        .route(&format!("{PAGES}/{{id}}"), get(detail))
}

/// GET /admin/reports/requests
async fn list(State(state): State<AppState>, session: Session) -> Response {
    let Some(store) = state.profiler() else {
        return render_not_found();
    };
    if let Err(refused) = require_permission(&state, &session, PERMISSION).await {
        return refused;
    }

    let mut context = tera::Context::new();
    context.insert("profiles", &store.recent());
    context.insert("capacity", &store.capacity());
    context.insert("show_sql_values", &store.show_sql_values());
    context.insert("path", PAGES);
    render_admin_template(
        &state,
        &get_user_context(&session, &state).await,
        "admin/reports/requests.html",
        context,
    )
    .await
}

/// GET /admin/reports/requests/{id}
async fn detail(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    let Some(store) = state.profiler() else {
        return render_not_found();
    };
    if let Err(refused) = require_permission(&state, &session, PERMISSION).await {
        return refused;
    }
    // Parsed here rather than by the extractor, so an id that is not a number
    // is the same 404 as one that has left the ring.
    let Some(profile) = id.parse::<u64>().ok().and_then(|id| store.get(id)) else {
        return render_not_found();
    };

    let mut context = tera::Context::new();
    context.insert("profile", profile.as_ref());
    context.insert("show_sql_values", &store.show_sql_values());
    context.insert("path", PAGES);
    render_admin_template(
        &state,
        &get_user_context(&session, &state).await,
        "admin/reports/request.html",
        context,
    )
    .await
}
