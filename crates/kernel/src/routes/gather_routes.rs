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
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower_sessions::Session;

use crate::gather::types::{GatherQuery, GatherRouteParam};
use crate::middleware::language::ResolvedLanguage;
use crate::models::stage::LIVE_STAGE_ID;
use crate::models::{Tag, User};
use crate::routes::gather::{ExecuteParams, GatherPageError};
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
                          requested: Option<Extension<crate::middleware::RequestedPath>>,
                          uri: OriginalUri,
                          path: Path<HashMap<String, String>>,
                          query_params: Query<HashMap<String, String>>| {
                        let config = config_clone.clone();
                        async move {
                            handle_gather_route(
                                state,
                                session,
                                resolved_lang,
                                requested.map(|Extension(r)| r.0),
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

/// Why a path segment could not be turned into a gather argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteParamError {
    /// The segment names nothing: an unknown tag slug or account name, or a
    /// segment the route does not carry. The caller answers 404.
    NotFound,
    /// The lookup itself failed. The caller answers 500.
    Failed,
}

/// Resolve a route's path segments to the gather's URL arguments.
///
/// The one place a `display.routes` parameter mapping is applied, for the
/// route's page and for a feed that binds the same segment
/// ([`crate::routes::feed`]):
///
/// - `tag_category`: the segment is a tag slug in that category, passed on as
///   the tag's id;
/// - `user`: the segment is an account name, passed on as the account's id;
/// - neither: the segment is passed on as it is.
///
/// A lookup that finds nothing is [`RouteParamError::NotFound`], so an unknown
/// tag or author is a 404 rather than an unfiltered or empty listing.
pub(crate) async fn resolve_route_params(
    state: &AppState,
    mappings: &[GatherRouteParam],
    segments: &HashMap<String, String>,
) -> Result<HashMap<String, String>, RouteParamError> {
    resolve_route_args(state, mappings, segments)
        .await
        .map(|route| route.params)
}

/// What a route's path segments resolve to: the gather's URL arguments, and
/// the term the first `tag_category` segment named, if any.
pub(crate) struct ResolvedRoute {
    pub params: HashMap<String, String>,
    /// The tag a term page is about, as the lookup found it: the page is titled
    /// by it (B160). No second query: it is the row the slug lookup returned.
    pub term: Option<Tag>,
}

/// [`resolve_route_params`], keeping the tag a `tag_category` segment named.
pub(crate) async fn resolve_route_args(
    state: &AppState,
    mappings: &[GatherRouteParam],
    segments: &HashMap<String, String>,
) -> Result<ResolvedRoute, RouteParamError> {
    let mut resolved = HashMap::new();
    let mut term: Option<Tag> = None;

    for mapping in mappings {
        // Path segment not present — should not happen with correct route
        // registration, but answer 404 to be safe.
        let raw_value = segments
            .get(&mapping.segment)
            .ok_or(RouteParamError::NotFound)?;

        if let Some(ref category_id) = mapping.tag_category {
            // Tag slug lookup: validate slug format, then resolve to UUID.
            if !is_valid_slug(raw_value) {
                return Err(RouteParamError::NotFound);
            }
            match Tag::find_by_slug(state.db(), category_id, raw_value).await {
                Ok(Some(tag)) => {
                    resolved.insert(mapping.param.clone(), tag.id.to_string());
                    if term.is_none() {
                        term = Some(tag);
                    }
                }
                Ok(None) => return Err(RouteParamError::NotFound),
                Err(_) => return Err(RouteParamError::Failed),
            }
        } else if mapping.user {
            // Account name lookup. Only an active account resolves: a blocked
            // one is not an author anyone can browse.
            if raw_value.is_empty() || raw_value.len() > 255 {
                return Err(RouteParamError::NotFound);
            }
            match User::find_by_name(state.db(), raw_value).await {
                Ok(Some(user)) if user.status == 1 => {
                    resolved.insert(mapping.param.clone(), user.id.to_string());
                }
                Ok(_) => return Err(RouteParamError::NotFound),
                Err(_) => return Err(RouteParamError::Failed),
            }
        } else {
            // Pass-through: forward the raw value as a query parameter.
            // Safety: the gather engine uses parameterized SQL queries (never
            // format!), and Tera auto-escapes HTML output. No additional
            // validation is applied here because legitimate values (e.g. city
            // names with spaces, accents, or apostrophes) must pass through.
            resolved.insert(mapping.param.clone(), raw_value.clone());
        }
    }

    Ok(ResolvedRoute {
        params: resolved,
        term,
    })
}

/// Handle a gather route alias request.
///
/// Extracts path segments, resolves tag slugs if needed, and renders the
/// gather query inline at the current URL (no redirect).
async fn handle_gather_route(
    State(state): State<AppState>,
    session: Session,
    resolved_lang: ResolvedLanguage,
    requested_path: Option<String>,
    OriginalUri(uri): OriginalUri,
    Path(segments): Path<HashMap<String, String>>,
    Query(extra_params): Query<HashMap<String, String>>,
    config: &RouteConfig,
) -> Response {
    let (resolved_params, term) = match resolve_route_args(&state, &config.params, &segments).await
    {
        Ok(resolved) => (resolved.params, resolved.term),
        Err(RouteParamError::NotFound) => return render_not_found(),
        Err(RouteParamError::Failed) => {
            return render_server_error("Failed to resolve the address");
        }
    };

    // Content negotiation: check format param before consuming extra_params
    let wants_json = extra_params.get("format").is_some_and(|f| f == "json");

    // Merge extra query params (e.g. page) with resolved params.
    // Resolved params take precedence over extra params.
    let mut all_filters: HashMap<String, String> = extra_params;
    all_filters.extend(resolved_params);
    all_filters.remove("format"); // Don't pass format to the query engine
    // The page, counted as the gather's pager counts (from 0 for a gather
    // that replaces a Drupal listing). The JSON form is the API's, from 1.
    let query = state.gather().get_query(&config.query_id);
    let raw_page = all_filters.remove("page");
    let page = match (&query, wants_json) {
        (Some(query), false) => {
            super::gather::requested_page(&query.display.pager, raw_page.as_deref())
        }
        _ => raw_page
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1)
            .max(1),
    };

    // Gather route aliases always show Live content — ignore any user-supplied
    // stage parameter to prevent anonymous access to internal stages.
    all_filters.remove("stage");
    let stage = LIVE_STAGE_ID.to_string();

    // The page advertises the feeds of this gather that take the same
    // argument it was reached with (`/blog/author/jeremy` →
    // `/blog/author/jeremy/feed.xml`).
    let feeds = query
        .map(|query| crate::routes::feed::argument_feed_links(&query, &segments))
        .unwrap_or_default();
    let params = ExecuteParams::new(page, stage, all_filters)
        .with_feeds(feeds)
        .with_term(term.as_ref());

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
        // JSON response: execute query and return structured data. Enforce
        // access for the requesting viewer (Story 3.4) — the aliased JSON path
        // must filter exactly like the HTML path.
        let viewer = crate::routes::item::get_user_context(&session, &state).await;
        match super::gather::execute_query_only(
            &state,
            &config.query_id,
            params,
            language,
            viewer,
            None,
        )
        .await
        {
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
            Err(e) => match e.downcast_ref::<crate::gather::GatherDenied>() {
                Some(denied) => super::gather::gather_denied_json(denied),
                None => {
                    tracing::error!(error = %e, "gather JSON query failed");
                    render_server_error("Query execution failed")
                }
            },
        }
    } else {
        match super::gather::execute_and_render(
            &state,
            &session,
            &config.query_id,
            params,
            &base_path,
            language,
            requested_path.as_deref(),
        )
        .await
        {
            Ok(Html(html)) => Html(html).into_response(),
            Err(GatherPageError::Denied(denied)) => super::gather::gather_denied_html(&denied),
            Err(GatherPageError::NotFound | GatherPageError::PastTheEnd) => render_not_found(),
            Err(GatherPageError::Failed(error)) => render_server_error(&error),
        }
    } // end else (HTML branch)
}
