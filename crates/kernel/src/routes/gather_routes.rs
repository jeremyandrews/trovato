//! Generic gather route aliases.
//!
//! Builds dynamic routes from gather query display configs at startup. Each
//! Each route entry in a query's `display.routes` array creates an HTTP route
//! that redirects to the corresponding gather URL with mapped query parameters.
//!
//! Two parameter mapping modes:
//! - **Pass-through:** path segment value is passed directly as a query param.
//! - **Tag slug lookup:** path segment is resolved to a tag UUID via the
//!   `category_tag.slug` column, then the UUID is passed as the query param.
//!
//! This replaces the former `ritrovo_topics.rs` module, which hard-coded
//! routes for `/topics/{slug}` and `/location/{country}[/{city}]`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::get,
};

use crate::gather::types::{GatherQuery, GatherRouteParam};
use crate::models::Tag;
use crate::state::AppState;

/// Configuration for a single registered gather route.
#[derive(Debug, Clone)]
struct RouteConfig {
    /// Gather query ID to redirect to.
    query_id: String,
    /// Parameter mappings from path segments to query params.
    params: Vec<GatherRouteParam>,
}

/// Build a router from all gather queries that have `display.routes` configured.
///
/// Called once at startup after gather queries are loaded from the database.
/// Each route is registered with a handler that captures its configuration.
pub fn build_gather_route_router(queries: &[GatherQuery]) -> Router<AppState> {
    let mut router = Router::new();

    for query in queries {
        for route in &query.display.routes {
            let config = Arc::new(RouteConfig {
                query_id: query.query_id.clone(),
                params: route.params.clone(),
            });

            // Validate path pattern before registering
            if route.path.is_empty() || !route.path.starts_with('/') {
                tracing::warn!(
                    query_id = %query.query_id,
                    path = %route.path,
                    "skipping gather route with invalid path"
                );
                continue;
            }

            tracing::info!(
                query_id = %query.query_id,
                path = %route.path,
                "registering gather route alias"
            );

            let config_clone = config.clone();
            router = router.route(
                &route.path,
                get(
                    move |state: State<AppState>,
                          path: Path<HashMap<String, String>>,
                          query_params: Query<HashMap<String, String>>| {
                        let config = config_clone.clone();
                        async move { handle_gather_route(state, path, query_params, &config).await }
                    },
                ),
            );
        }
    }

    router
}

/// Handle a gather route alias request.
///
/// Extracts path segments, resolves tag slugs if needed, and redirects
/// to the gather URL with the appropriate query parameters.
async fn handle_gather_route(
    State(state): State<AppState>,
    Path(segments): Path<HashMap<String, String>>,
    Query(extra_params): Query<HashMap<String, String>>,
    config: &RouteConfig,
) -> Response {
    let mut gather_params: Vec<(String, String)> = Vec::new();

    for mapping in &config.params {
        let Some(raw_value) = segments.get(&mapping.segment) else {
            // Path segment not present — should not happen with correct route
            // registration, but return 404 to be safe.
            return StatusCode::NOT_FOUND.into_response();
        };

        if let Some(ref category_id) = mapping.tag_category {
            // Tag slug lookup: validate slug format, then resolve to UUID.
            if raw_value.is_empty()
                || raw_value.len() > 128
                || !raw_value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return StatusCode::NOT_FOUND.into_response();
            }

            match Tag::find_by_slug(state.db(), category_id, raw_value).await {
                Ok(Some(tag)) => {
                    gather_params.push((mapping.param.clone(), tag.id.to_string()));
                }
                Ok(None) => return StatusCode::NOT_FOUND.into_response(),
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        } else {
            // Pass-through: URL-encode and pass directly.
            gather_params.push((mapping.param.clone(), raw_value.clone()));
        }
    }

    // Build the redirect URL.
    let mut url = format!("/gather/{}", config.query_id);
    let mut first = true;

    for (key, value) in &gather_params {
        url.push(if first { '?' } else { '&' });
        first = false;
        url.push_str(&urlencoding::encode(key));
        url.push('=');
        url.push_str(&urlencoding::encode(value));
    }

    // Preserve extra query params (e.g. page).
    for (key, value) in &extra_params {
        url.push(if first { '?' } else { '&' });
        first = false;
        url.push_str(&urlencoding::encode(key));
        url.push('=');
        url.push_str(&urlencoding::encode(value));
    }

    Redirect::temporary(&url).into_response()
}
