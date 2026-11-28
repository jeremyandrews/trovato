//! Search route handlers.

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tera::Context;
use uuid::Uuid;

use tower_sessions::Session;

use crate::pagination::{Page, PageParams};
use crate::routes::helpers::{read_stages, session_stage};
use crate::state::AppState;

/// Create the search router.
pub fn router() -> Router<AppState> {
    Router::new().route("/search", get(search_html))
}

/// The search JSON API, under `/api/v1` ([`super::api`]).
///
/// `GET /api/v1/search` is the one JSON search: the unversioned
/// `/api/search` is its deprecated alias, answered by the same handler.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("search")
        .get(
            "/search",
            search_json,
            doc("Full-text search across content")
                .query("q", "Search query; an empty one answers an empty page")
                .paged(SEARCH_PER_PAGE, SEARCH_MAX_PER_PAGE)
                .describe(
                    "Searches Live, or for a holder of `use stages` the session's stage \
                     with Live beneath it, and returns only what the caller may view.",
                )
                .alias(),
        )
        .post(
            "/search/log",
            log_browser_search,
            doc("Count a search the browser ran in Pagefind (analytics beacon)").alias(),
        )
}

/// The permission both search routes ask for (B160). Drupal's name for it, so
/// a migrated role keeps or lacks it as it did.
pub const SEARCH_PERMISSION: &str = "search content";

/// The id of the site search form, on the search page and on the page-not-found
/// page.
pub const SEARCH_FORM_ID: &str = "search_form";

/// The site search form (B83c): a GET form to `/search` holding `field` as
/// its search field `q` (an `input("search")`, labelled as its page needs),
/// with a Search button. The search page and the page-not-found page each
/// build it through the form pipeline; as a GET form it carries no token and
/// saves nothing.
pub fn search_form(field: crate::form::FormElement) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    Form::new(SEARCH_FORM_ID)
        .method("get")
        .action("/search")
        .element("q", field.weight(0))
        .element("op", FormElement::submit("Search").weight(1))
}

/// Default page size of the JSON search.
const SEARCH_PER_PAGE: i64 = 10;

/// Largest page size of the JSON search.
const SEARCH_MAX_PER_PAGE: i64 = 50;

/// Longest query the beacon accepts, in bytes, before normalization.
const MAX_BEACON_QUERY_BYTES: usize = 500;

/// Largest result count the beacon accepts.
const MAX_BEACON_RESULTS: i64 = 100_000;

/// A search the browser ran in Pagefind, reported for analytics.
#[derive(Debug, Deserialize)]
struct SearchLog {
    query: String,
    results: i64,
}

/// `POST /api/v1/search/log`: count a browser search.
///
/// Browser searches run entirely in Pagefind and never reach the server, so
/// `scolta.js` reports each one here with `navigator.sendBeacon`, once, after
/// its results render. Validated, counted in memory, answered 204; nothing
/// touches the database. No CSRF token: the request changes nothing a visitor
/// owns, and the rate limiter files it under the `search` bucket.
async fn log_browser_search(
    State(state): State<AppState>,
    Json(body): Json<SearchLog>,
) -> Response {
    if body.query.len() > MAX_BEACON_QUERY_BYTES {
        return crate::error::AppError::bad_request("query is too long").into_response();
    }
    let Ok(results) = u64::try_from(body.results) else {
        return crate::error::AppError::bad_request("results must not be negative").into_response();
    };
    if body.results > MAX_BEACON_RESULTS {
        return crate::error::AppError::bad_request("results is out of range").into_response();
    }
    let settings = crate::search::SearchSettings::load(state.db(), state.cache()).await;
    state
        .search()
        .analytics()
        .record(&settings, &body.query, results);
    StatusCode::NO_CONTENT.into_response()
}

/// Search query parameters.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// Search query string.
    pub q: Option<String>,
    /// Page number (1-indexed).
    #[serde(default = "default_page")]
    pub page: i64,
    /// Results per page.
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_page() -> i64 {
    1
}

fn default_limit() -> i64 {
    10
}

/// Single search result in JSON format.
#[derive(Debug, Serialize)]
pub struct SearchResultJson {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub item_type: String,
    pub title: String,
    pub rank: f32,
    pub snippet: Option<String>,
    pub url: String,
}

/// HTML search page.
async fn search_html(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<SearchQuery>,
) -> Response {
    let query = params.q.clone().unwrap_or_default();
    let page = params.page.max(1);
    let limit = params.limit.clamp(1, 50);
    let offset = (page - 1) * limit;

    // Live, or the session's stage with Live beneath it for a holder of
    // `use stages`: the one resolver every read path uses.
    let viewer = crate::routes::item::get_user_context(&session, &state).await;
    if !viewer.can(SEARCH_PERMISSION) {
        return super::helpers::render_forbidden("You are not allowed to search this site.");
    }
    // The signed-in user's id, from the viewer the guards loaded: a deleted
    // or blocked account is nobody here too.
    let user_id = viewer.authenticated.then_some(viewer.id);
    let stage_ids = read_stages(&state, &viewer, session_stage(&session).await).await;

    // Execute search
    let mut results = match state
        .search()
        .search(&query, &stage_ids, user_id, limit, offset)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "search failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Search error".to_string()),
            )
                .into_response();
        }
    };

    // FR-8 Story 3.7 — route result rows through the shared access seam: drop
    // items the viewer cannot see and redact restricted-field snippets.
    results.results = state
        .items()
        .filter_search_results(results.results, &viewer)
        .await;

    // Calculate pagination
    let total_pages = (results.total + limit - 1) / limit;

    // The search box again, through the form pipeline.
    let form = search_form(
        crate::form::FormElement::input("search")
            .default_value(query.clone())
            .placeholder("Search...")
            .attribute("aria-label", "Search"),
    )
    .attribute("class", "search-form");
    let search_form = match super::admin_form::render_as(&state, &session, &viewer, form, &[]).await
    {
        Ok(html) => html,
        Err(refusal) => return refusal,
    };

    // Build template context
    let mut context = Context::new();
    context.insert("search_form", &search_form);
    context.insert("query", &query);
    // Each snippet is already HTML, escaped exactly once by
    // `crate::search::snippet::render`; the template prints it `| safe`.
    context.insert("results", &results.results);
    let total = results.total;
    context.insert("total", &total);
    // A query with no word left to search for (`a`, `it's`, `&`): the page
    // says so instead of only "no results".
    context.insert("no_terms", &(results.no_terms && !query.trim().is_empty()));
    context.insert("page", &page);
    context.insert("limit", &limit);
    context.insert("total_pages", &total_pages);
    context.insert("has_prev", &(page > 1));
    context.insert("has_next", &(page < total_pages));
    context.insert("prev_page", &(page - 1));
    context.insert("next_page", &(page + 1));

    // Inject site context for page layout (header, nav, footer)
    super::helpers::inject_site_context(&state, &session, &mut context, "/search").await;

    // The browser's ranking and AI settings, from the one settings model (read
    // through the cache). The site name comes from the context just injected
    // rather than a second read.
    let site_name = context
        .get("site_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Trovato")
        .to_string();
    let settings = crate::search::SearchSettings::load(state.db(), state.cache()).await;
    context.insert("scolta_config", &settings.scolta_config(&site_name));

    // Render template
    match state.theme().tera().render("search.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "failed to render search template");
            super::helpers::render_server_error("Search results could not be displayed.")
        }
    }
}

/// JSON search endpoint: `GET /api/v1/search?q=…&page=…&per_page=…`.
///
/// One page of [`SearchResultJson`] in the API's list shape. An empty `q`
/// answers an empty page without searching.
async fn search_json(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<SearchJsonQuery>,
    Query(paging): Query<PageParams>,
) -> Response {
    let window = paging.window(SEARCH_PER_PAGE, SEARCH_MAX_PER_PAGE);
    let query = params.q.unwrap_or_default();
    if query.len() > crate::search::MAX_QUERY_LENGTH {
        return crate::error::AppError::bad_request(format!(
            "q exceeds maximum length of {} characters",
            crate::search::MAX_QUERY_LENGTH
        ))
        .into_response();
    }
    if query.trim().is_empty() {
        return Json(Page::<SearchResultJson>::empty(window)).into_response();
    }

    // Live, or the session's stage with Live beneath it for a holder of
    // `use stages`: the one resolver every read path uses.
    let viewer = crate::routes::item::get_user_context(&session, &state).await;
    if !viewer.can(SEARCH_PERMISSION) {
        return super::helpers::refusal_for(
            &viewer,
            format!("Permission required: {SEARCH_PERMISSION}"),
        )
        .into_response();
    }
    // The signed-in user's id, from the viewer the guards loaded: a deleted
    // or blocked account is nobody here too.
    let user_id = viewer.authenticated.then_some(viewer.id);
    let stage_ids = read_stages(&state, &viewer, session_stage(&session).await).await;

    // Execute search
    let mut results = match state
        .search()
        .search(&query, &stage_ids, user_id, window.per_page, window.offset)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "search failed");
            return crate::error::AppError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Search failed",
            )
            .into_response();
        }
    };

    // FR-8 Story 3.7 — route result rows through the shared access seam.
    results.results = state
        .items()
        .filter_search_results(results.results, &viewer)
        .await;

    // A snippet is HTML (text escaped once, `<mark>` around the matches),
    // built by `crate::search::snippet::render`.
    let data = results
        .results
        .into_iter()
        .map(|r| SearchResultJson {
            url: format!("/item/{}", r.id),
            id: r.id,
            item_type: r.item_type,
            title: r.title,
            rank: r.rank,
            snippet: r.snippet,
        })
        .collect();
    Json(Page::new(
        data,
        u64::try_from(results.total).unwrap_or(0),
        window,
    ))
    .into_response()
}

/// The JSON search's own parameter; the page comes from [`PageParams`].
#[derive(Debug, Deserialize)]
struct SearchJsonQuery {
    q: Option<String>,
}
