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
//! | `GET /conferences` | `ritrovo.upcoming_conferences` |
//! | `GET /cfps` | `ritrovo.open_cfps` |
//! | `GET /location/:country` | `ritrovo.by_country` |
//! | `GET /location/:country/:city` | `ritrovo.by_city` |
//!
//! The topic route resolves the human-readable slug to the `category_tag` UUID
//! stored in `ritrovo_state` by the importer's `tap_install` tap, then
//! redirects to the corresponding gather URL with the UUID as a query
//! parameter.  Location routes redirect straight to the gather URL with the
//! URL-encoded path segments as query parameters.
//!
//! `/conferences` and `/cfps` redirect to their canonical gather URLs and
//! preserve any existing query-string parameters (exposed filter values,
//! pagination).

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Redirect,
    routing::get,
};
use std::collections::HashMap;

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

/// Upcoming conferences listing (redirects to the gather page).
///
/// Preserves any query-string parameters so that bookmarked filter URLs
/// like `/conferences?country=Germany&page=2` work correctly after redirect.
async fn conferences(Query(params): Query<HashMap<String, String>>) -> Redirect {
    let mut url = "/gather/ritrovo.upcoming_conferences".to_string();
    append_params(&mut url, &params);
    Redirect::temporary(&url)
}

/// Open CFPs listing (redirects to the gather page).
async fn cfps(Query(params): Query<HashMap<String, String>>) -> Redirect {
    let mut url = "/gather/ritrovo.open_cfps".to_string();
    append_params(&mut url, &params);
    Redirect::temporary(&url)
}

/// Browse conferences by country.
///
/// Redirects to `/gather/ritrovo.by_country?country=<encoded>`.
async fn by_country(
    Path(country): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Redirect {
    let mut url = format!(
        "/gather/ritrovo.by_country?country={}",
        urlencoding::encode(&country),
    );
    append_params(&mut url, &params);
    Redirect::temporary(&url)
}

/// Browse conferences by country and city.
///
/// Redirects to `/gather/ritrovo.by_city?country=<encoded>&city=<encoded>`.
async fn by_country_city(
    Path((country, city)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Redirect {
    let mut url = format!(
        "/gather/ritrovo.by_city?country={}&city={}",
        urlencoding::encode(&country),
        urlencoding::encode(&city),
    );
    append_params(&mut url, &params);
    Redirect::temporary(&url)
}

// ─── Helpers ──────────────────────────────────────────────────────────

/// Append URL query parameters to `url`, preceded by `&`.
///
/// Skips keys that are already part of the base URL to avoid duplicates.
fn append_params(url: &mut String, params: &HashMap<String, String>) {
    for (k, v) in params {
        url.push('&');
        url.push_str(&urlencoding::encode(k));
        url.push('=');
        url.push_str(&urlencoding::encode(v));
    }
}
