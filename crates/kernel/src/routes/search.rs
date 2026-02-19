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

use crate::routes::auth::SESSION_USER_ID;
use crate::routes::helpers::html_escape;
use crate::state::AppState;

/// Create the search router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/search", get(search_html))
        .route("/api/search", get(search_json))
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

/// JSON search response.
#[derive(Debug, Serialize)]
pub struct SearchJsonResponse {
    pub query: String,
    pub results: Vec<SearchResultJson>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub total_pages: i64,
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

    // Get user ID if logged in
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    // Execute search
    let results = match state.search().search(&query, user_id, limit, offset).await {
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

    // Calculate pagination
    let total_pages = (results.total + limit - 1) / limit;

    // Build template context
    let mut context = Context::new();
    context.insert("query", &query);
    context.insert("results", &results.results);
    context.insert("total", &results.total);
    context.insert("page", &page);
    context.insert("limit", &limit);
    context.insert("total_pages", &total_pages);
    context.insert("has_prev", &(page > 1));
    context.insert("has_next", &(page < total_pages));
    context.insert("prev_page", &(page - 1));
    context.insert("next_page", &(page + 1));

    // Render template
    match state.theme().tera().render("search.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render search template");
            // Fallback to simple HTML
            let html =
                render_fallback_search(&query, &results.results, results.total, page, total_pages);
            Html(html).into_response()
        }
    }
}

/// JSON search endpoint.
async fn search_json(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<SearchQuery>,
) -> Response {
    let query = params.q.clone().unwrap_or_default();
    let page = params.page.max(1);
    let limit = params.limit.clamp(1, 50);
    let offset = (page - 1) * limit;

    // Get user ID if logged in
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    // Execute search
    let results = match state.search().search(&query, user_id, limit, offset).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "search failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Search failed"
                })),
            )
                .into_response();
        }
    };

    // Calculate pagination
    let total_pages = (results.total + limit - 1) / limit;

    // Build response
    let response = SearchJsonResponse {
        query,
        results: results
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
            .collect(),
        total: results.total,
        page,
        limit,
        total_pages,
    };

    Json(response).into_response()
}

/// Render fallback search HTML when template is unavailable.
fn render_fallback_search(
    query: &str,
    results: &[crate::search::SearchResult],
    total: i64,
    page: i64,
    total_pages: i64,
) -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Search Results</title>
    <style>
        body { font-family: sans-serif; max-width: 800px; margin: 2rem auto; padding: 0 1rem; }
        .result { margin: 1.5rem 0; padding: 1rem; border: 1px solid #ddd; border-radius: 4px; }
        .result h3 { margin: 0 0 0.5rem; }
        .result h3 a { color: #1a0dab; text-decoration: none; }
        .result h3 a:hover { text-decoration: underline; }
        .snippet { color: #545454; }
        .meta { color: #006621; font-size: 0.9rem; }
        .pagination { margin: 2rem 0; text-align: center; }
        .pagination a { margin: 0 0.5rem; }
        form { margin-bottom: 2rem; }
        input[type=search] { width: 300px; padding: 0.5rem; font-size: 1rem; }
        button { padding: 0.5rem 1rem; font-size: 1rem; }
    </style>
</head>
<body>
    <h1>Search</h1>
    <form action="/search" method="get">
        <input type="search" name="q" value=""#,
    );
    html.push_str(&html_escape(query));
    html.push_str(
        r#"" placeholder="Search...">
        <button type="submit">Search</button>
    </form>
"#,
    );

    if !query.is_empty() {
        html.push_str(&format!(
            "<p>Found {} results for \"{}\"</p>\n",
            total,
            html_escape(query)
        ));

        for result in results {
            html.push_str("<div class=\"result\">\n");
            html.push_str(&format!(
                "    <h3><a href=\"/item/{}\">{}</a></h3>\n",
                result.id,
                html_escape(&result.title)
            ));
            html.push_str(&format!(
                "    <div class=\"meta\">{}</div>\n",
                result.item_type
            ));
            if let Some(snippet) = &result.snippet {
                html.push_str(&format!(
                    "    <div class=\"snippet\">{snippet}</div>\n" // Already contains safe HTML from ts_headline
                ));
            }
            html.push_str("</div>\n");
        }

        // Pagination
        if total_pages > 1 {
            html.push_str("<div class=\"pagination\">\n");
            if page > 1 {
                html.push_str(&format!(
                    "    <a href=\"/search?q={}&page={}\">&laquo; Previous</a>\n",
                    urlencoding::encode(query),
                    page - 1
                ));
            }
            html.push_str(&format!("    Page {page} of {total_pages}\n"));
            if page < total_pages {
                html.push_str(&format!(
                    "    <a href=\"/search?q={}&page={}\">Next &raquo;</a>\n",
                    urlencoding::encode(query),
                    page + 1
                ));
            }
            html.push_str("</div>\n");
        }
    }

    html.push_str("</body></html>");
    html
}
