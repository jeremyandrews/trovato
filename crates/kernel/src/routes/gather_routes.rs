//! Generic gather route aliases.
//!
//! Builds dynamic routes from gather query display configs at startup. Each
//! route entry in a query's `display.routes` array creates an HTTP route that
//! renders the corresponding gather query inline with mapped query parameters.
//!
//! Two parameter mapping modes:
//! - **Pass-through:** path segment value is passed directly as a query param.
//! - **Tag slug lookup:** path segment is resolved to a tag UUID via the
//!   `category_tag.slug` column, then the UUID is passed as the query param.
//!
//! This replaces the former `ritrovo_topics.rs` module, which hard-coded
//! routes for `/topics/{slug}` and `/location/{country}[/{city}]`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower_sessions::Session;

use crate::gather::types::{GatherQuery, GatherRouteParam};
use crate::middleware::language::ResolvedLanguage;
use crate::models::Tag;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::gather::ExecuteParams;
use crate::routes::helpers::{is_valid_slug, render_not_found, render_server_error};
use crate::state::AppState;

/// Configuration for a single registered gather route.
#[derive(Debug, Clone)]
struct RouteConfig {
    /// Gather query ID to render.
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
    let mut registered_paths: HashSet<String> = HashSet::new();

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

            // Guard against duplicate paths — axum panics on route conflicts.
            if !registered_paths.insert(route.path.clone()) {
                tracing::warn!(
                    query_id = %query.query_id,
                    path = %route.path,
                    "skipping duplicate gather route path"
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
                          session: Session,
                          Extension(resolved_lang): Extension<ResolvedLanguage>,
                          uri: OriginalUri,
                          path: Path<HashMap<String, String>>,
                          query_params: Query<HashMap<String, String>>| {
                        let config = config_clone.clone();
                        async move {
                            handle_gather_route(
                                state,
                                session,
                                resolved_lang,
                                uri,
                                path,
                                query_params,
                                &config,
                            )
                            .await
                        }
                    },
                ),
            );
        }
    }

    router
}

/// Handle a gather route alias request.
///
/// Extracts path segments, resolves tag slugs if needed, and renders the
/// gather query inline at the current URL (no redirect).
async fn handle_gather_route(
    State(state): State<AppState>,
    session: Session,
    resolved_lang: ResolvedLanguage,
    OriginalUri(uri): OriginalUri,
    Path(segments): Path<HashMap<String, String>>,
    Query(extra_params): Query<HashMap<String, String>>,
    config: &RouteConfig,
) -> Response {
    let mut resolved_params: HashMap<String, String> = HashMap::new();

    for mapping in &config.params {
        let Some(raw_value) = segments.get(&mapping.segment) else {
            // Path segment not present — should not happen with correct route
            // registration, but return 404 to be safe.
            return render_not_found();
        };

        if let Some(ref category_id) = mapping.tag_category {
            // Tag slug lookup: validate slug format, then resolve to UUID.
            if !is_valid_slug(raw_value) {
                return render_not_found();
            }

            match Tag::find_by_slug(state.db(), category_id, raw_value).await {
                Ok(Some(tag)) => {
                    resolved_params.insert(mapping.param.clone(), tag.id.to_string());
                }
                Ok(None) => return render_not_found(),
                Err(_) => return render_server_error("Failed to look up tag"),
            }
        } else {
            // Pass-through: forward the raw value as a query parameter.
            // Safety: the gather engine uses parameterized SQL queries (never
            // format!), and Tera auto-escapes HTML output. No additional
            // validation is applied here because legitimate values (e.g. city
            // names with spaces, accents, or apostrophes) must pass through.
            resolved_params.insert(mapping.param.clone(), raw_value.clone());
        }
    }

    // Content negotiation: check format param before consuming extra_params
    let wants_json = extra_params.get("format").is_some_and(|f| f == "json");

    // Merge extra query params (e.g. page) with resolved params.
    // Resolved params take precedence over extra params.
    let mut all_filters: HashMap<String, String> = extra_params;
    all_filters.extend(resolved_params);
    all_filters.remove("format"); // Don't pass format to the query engine

    // Extract page from query params, clamping to minimum 1.
    let page = all_filters
        .remove("page")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);

    // Gather route aliases always show Live content — ignore any user-supplied
    // stage parameter to prevent anonymous access to internal stages.
    all_filters.remove("stage");
    let stage = LIVE_STAGE_ID.to_string();

    let params = ExecuteParams::new(page, stage, all_filters);

    // Use the request path (without query string) as the base path so that
    // pager links and form actions stay on the pretty URL.
    let base_path = uri.path().to_string();

    // Resolve language: skip the translation JOIN for the default language.
    let language = if resolved_lang.0 != state.default_language() {
        Some(resolved_lang.0)
    } else {
        None
    };

    if wants_json {
        // JSON response: execute query and return structured data
        match super::gather::execute_query_only(&state, &config.query_id, params, language).await {
            Ok(result) => Json(serde_json::json!({
                "items": result.items,
                "pager": {
                    "current_page": result.page,
                    "total_pages": result.total_pages,
                    "total_items": result.total,
                    "per_page": result.per_page,
                },
                "query": {
                    "name": config.query_id,
                },
            }))
            .into_response(),
            Err(e) => {
                tracing::error!(error = %e, "gather JSON query failed");
                render_server_error("Query execution failed")
            }
        }
    } else {
        match super::gather::execute_and_render(
            &state,
            &session,
            &config.query_id,
            params,
            &base_path,
            language,
        )
        .await
        {
            Ok(Html(html)) => Html(html).into_response(),
            Err((status, Json(err))) => {
                if status == StatusCode::NOT_FOUND {
                    render_not_found()
                } else {
                    render_server_error(&err.error)
                }
            }
        }
    } // end else (HTML branch)
}
