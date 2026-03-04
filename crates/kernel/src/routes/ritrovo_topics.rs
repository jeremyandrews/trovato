//! Ritrovo tutorial browse routes.
//!
//! Provides parameterised URL routes for the topic and location browse pages
//! that the Ritrovo importer plugin creates.  All routes in this module are
//! gated by the `ritrovo_importer` plugin: they return 404 when that plugin
//! is disabled.
//!
//! ## Route overview
//!
//! | Path | Gather query |
//! |------|-------------|
//! | `GET /topics/:slug` | `ritrovo.by_topic` (UUID looked up from ritrovo_state) |
//! | `GET /conferences` | `ritrovo.upcoming_conferences` (rendered in-place) |
//! | `GET /cfps` | `ritrovo.open_cfps` (rendered in-place) |
//! | `GET /location/:country` | `ritrovo.by_country` |
//! | `GET /location/:country/:city` | `ritrovo.by_city` |
//!
//! The topic and location routes resolve path segments to gather filter values
//! and redirect to the corresponding gather URLs.  `/conferences` and `/cfps`
//! render their gather queries directly under their own paths so that pager
//! links, filter forms, and browser history all stay on the friendly URL.

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, Json, Redirect},
    routing::get,
};
use std::collections::HashMap;
use tower_sessions::Session;

use crate::routes::gather::{ExecuteParams, execute_and_render};
use crate::state::AppState;

/// Build the router for Ritrovo browse routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/topics/{slug}", get(by_topic))
        .route("/conferences", get(conferences))
        .route("/cfps", get(cfps))
        .route("/location/{country}", get(by_country))
        .route("/location/{country}/{city}", get(by_country_city))
}

// ─── Handlers ─────────────────────────────────────────────────────────

/// Browse conferences by topic.
///
/// Resolves `slug` to a `category_tag` UUID via `ritrovo_state`, then
/// redirects to `/gather/ritrovo.by_topic?topic=<uuid>` so that the gather
/// engine can apply the `HasTagOrDescendants` filter.
///
/// Returns 404 when the slug is not found in the taxonomy.
async fn by_topic(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, StatusCode> {
    // Validate slug to prevent DB-level issues before parameterized query.
    if slug.is_empty()
        || slug.len() > 128
        || !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(StatusCode::NOT_FOUND);
    }

    // Look up the term UUID from ritrovo_state.
    let state_key = format!("topic_term.{slug}");
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM ritrovo_state WHERE name = $1")
        .bind(&state_key)
        .fetch_optional(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (uuid,) = row.ok_or(StatusCode::NOT_FOUND)?;

    // Build redirect URL, preserving any extra query params (e.g. page).
    let mut gather_url = format!(
        "/gather/ritrovo.by_topic?topic={}",
        urlencoding::encode(&uuid)
    );
    for (k, v) in &params {
        gather_url.push('&');
        gather_url.push_str(&urlencoding::encode(k));
        gather_url.push('=');
        gather_url.push_str(&urlencoding::encode(v));
    }

    Ok(Redirect::temporary(&gather_url))
}

/// Upcoming conferences listing.
///
/// Renders the `ritrovo.upcoming_conferences` gather query directly under
/// `/conferences` so that pager links, filter forms, and browser history
/// all stay on this path rather than bouncing to `/gather/…`.
async fn conferences(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<ExecuteParams>,
) -> Result<Html<String>, (StatusCode, Json<crate::routes::helpers::JsonError>)> {
    execute_and_render(&state, &session, "ritrovo.upcoming_conferences", params, "/conferences")
        .await
}

/// Open CFPs listing.
///
/// Renders the `ritrovo.open_cfps` gather query directly under `/cfps`.
async fn cfps(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<ExecuteParams>,
) -> Result<Html<String>, (StatusCode, Json<crate::routes::helpers::JsonError>)> {
    execute_and_render(&state, &session, "ritrovo.open_cfps", params, "/cfps").await
}

/// Browse conferences by country.
///
/// Redirects to `/gather/ritrovo.by_country?country=<encoded>`.
async fn by_country(
    Path(country): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Redirect {
    let extra: String = params
        .iter()
        .map(|(k, v)| {
            format!(
                "&{}={}",
                urlencoding::encode(k),
                urlencoding::encode(v)
            )
        })
        .collect();
    Redirect::temporary(&format!(
        "/gather/ritrovo.by_country?country={}{}",
        urlencoding::encode(&country),
        extra
    ))
}

/// Browse conferences by country and city.
///
/// Redirects to `/gather/ritrovo.by_city?country=<encoded>&city=<encoded>`.
async fn by_country_city(
    Path((country, city)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Redirect {
    let extra: String = params
        .iter()
        .map(|(k, v)| {
            format!(
                "&{}={}",
                urlencoding::encode(k),
                urlencoding::encode(v)
            )
        })
        .collect();
    Redirect::temporary(&format!(
        "/gather/ritrovo.by_city?country={}&city={}{}",
        urlencoding::encode(&country),
        urlencoding::encode(&city),
        extra
    ))
}
