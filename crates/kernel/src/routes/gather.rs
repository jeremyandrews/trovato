//! Gather query API routes.
//!
//! REST endpoints for executing view queries.

use crate::gather::{FilterValue, GatherView, ViewDefinition, ViewDisplay};
use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, Json},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::helpers::html_escape as escape_html;

/// Create the gather router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/views", get(list_views))
        .route("/api/view/{view_id}", get(get_view))
        .route("/api/view/{view_id}/execute", get(execute_view))
        .route("/api/gather/query", post(execute_adhoc_query))
        .route("/gather/{view_id}", get(render_view_html))
}

// -------------------------------------------------------------------------
// Response types
// -------------------------------------------------------------------------

#[derive(Serialize)]
struct ViewSummary {
    view_id: String,
    label: String,
    description: Option<String>,
    plugin: String,
}

#[derive(Serialize)]
struct ViewResponse {
    view_id: String,
    label: String,
    description: Option<String>,
    definition: ViewDefinition,
    display: ViewDisplay,
    plugin: String,
    created: i64,
    changed: i64,
}

#[derive(Serialize)]
struct GatherResultResponse {
    items: Vec<serde_json::Value>,
    total: u64,
    page: u32,
    per_page: u32,
    total_pages: u32,
    has_next: bool,
    has_prev: bool,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

// -------------------------------------------------------------------------
// Request types
// -------------------------------------------------------------------------

#[derive(Deserialize)]
struct ExecuteParams {
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_stage")]
    stage: String,
    /// Exposed filter values as JSON-encoded strings
    #[serde(flatten)]
    filters: HashMap<String, String>,
}

fn default_page() -> u32 {
    1
}

fn default_stage() -> String {
    "live".to_string()
}

#[derive(Deserialize)]
struct AdhocQueryRequest {
    definition: ViewDefinition,
    display: ViewDisplay,
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_stage")]
    stage: String,
    #[serde(default)]
    filters: HashMap<String, serde_json::Value>,
}

// -------------------------------------------------------------------------
// Handlers
// -------------------------------------------------------------------------

async fn list_views(
    State(state): State<AppState>,
) -> Result<Json<Vec<ViewSummary>>, (StatusCode, Json<ErrorResponse>)> {
    let views = state.gather().list_views();

    Ok(Json(
        views
            .into_iter()
            .map(|v| ViewSummary {
                view_id: v.view_id,
                label: v.label,
                description: v.description,
                plugin: v.plugin,
            })
            .collect(),
    ))
}

async fn get_view(
    State(state): State<AppState>,
    Path(view_id): Path<String>,
) -> Result<Json<ViewResponse>, (StatusCode, Json<ErrorResponse>)> {
    let view = state.gather().get_view(&view_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "view not found".to_string(),
            }),
        )
    })?;

    Ok(Json(ViewResponse {
        view_id: view.view_id,
        label: view.label,
        description: view.description,
        definition: view.definition,
        display: view.display,
        plugin: view.plugin,
        created: view.created,
        changed: view.changed,
    }))
}

async fn execute_view(
    State(state): State<AppState>,
    Path(view_id): Path<String>,
    Query(params): Query<ExecuteParams>,
) -> Result<Json<GatherResultResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Parse exposed filter values
    let exposed_filters = parse_filter_params(&params.filters);

    let result = state
        .gather()
        .execute(&view_id, params.page, exposed_filters, &params.stage)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(Json(GatherResultResponse {
        items: result.items,
        total: result.total,
        page: result.page,
        per_page: result.per_page,
        total_pages: result.total_pages,
        has_next: result.has_next,
        has_prev: result.has_prev,
    }))
}

async fn execute_adhoc_query(
    State(state): State<AppState>,
    Json(request): Json<AdhocQueryRequest>,
) -> Result<Json<GatherResultResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Convert JSON filter values to FilterValue
    let exposed_filters: HashMap<String, FilterValue> = request
        .filters
        .into_iter()
        .filter_map(|(k, v)| json_to_filter_value(v).map(|fv| (k, fv)))
        .collect();

    let result = state
        .gather()
        .execute_definition(
            &request.definition,
            &request.display,
            request.page,
            exposed_filters,
            &request.stage,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(Json(GatherResultResponse {
        items: result.items,
        total: result.total,
        page: result.page,
        per_page: result.per_page,
        total_pages: result.total_pages,
        has_next: result.has_next,
        has_prev: result.has_prev,
    }))
}

async fn render_view_html(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    Path(view_id): Path<String>,
    Query(params): Query<ExecuteParams>,
) -> Result<Html<String>, (StatusCode, Json<ErrorResponse>)> {
    let view = state.gather().get_view(&view_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "view not found".to_string(),
            }),
        )
    })?;

    let exposed_filters = parse_filter_params(&params.filters);

    let result = state
        .gather()
        .execute(&view_id, params.page, exposed_filters, &params.stage)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    // Render gather content (either via theme template or fallback HTML)
    let content_html = render_gather_with_theme(&state, &view, &result)
        .unwrap_or_else(|| render_gather_content_html(&view, &result));

    // Wrap in page layout with site context
    let gather_path = format!("/gather/{}", view_id);
    let mut context = tera::Context::new();
    super::helpers::inject_site_context(&state, &session, &mut context).await;

    let page_html = state
        .theme()
        .render_page(&gather_path, &view.label, &content_html, &mut context)
        .unwrap_or_else(|_| render_gather_html(&view, &result));

    Ok(Html(page_html))
}

fn render_gather_with_theme(
    state: &AppState,
    view: &GatherView,
    result: &crate::gather::GatherResult,
) -> Option<String> {
    // Try to find a template for this view
    let suggestions = [
        format!("gather/view--{}.html", view.view_id),
        format!("gather/view--{}.html", view.display.format.as_str()),
        "gather/view.html".to_string(),
    ];

    let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();
    let template = state.theme().resolve_template(&suggestion_refs)?;

    // Build context
    let mut context = tera::Context::new();
    context.insert("view", view);
    context.insert("rows", &result.items);
    context.insert("total", &result.total);
    context.insert("page", &result.page);
    context.insert("per_page", &result.per_page);
    context.insert("total_pages", &result.total_pages);
    context.insert("has_next", &result.has_next);
    context.insert("has_prev", &result.has_prev);

    // Pager info
    if view.display.pager.enabled && result.total_pages > 1 {
        context.insert(
            "pager",
            &serde_json::json!({
                "current_page": result.page,
                "total_pages": result.total_pages,
                "base_url": format!("/gather/{}", view.view_id),
            }),
        );
    }

    state.theme().tera().render(&template, &context).ok()
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn parse_filter_params(params: &HashMap<String, String>) -> HashMap<String, FilterValue> {
    params
        .iter()
        .filter(|(k, _)| !["page", "stage"].contains(&k.as_str()))
        .filter_map(|(k, v)| {
            // Try to parse as JSON, otherwise treat as string
            let filter_value = if let Ok(json) = serde_json::from_str::<serde_json::Value>(v) {
                json_to_filter_value(json)
            } else {
                Some(FilterValue::String(v.clone()))
            };
            filter_value.map(|fv| (k.clone(), fv))
        })
        .collect()
}

fn json_to_filter_value(value: serde_json::Value) -> Option<FilterValue> {
    match value {
        serde_json::Value::String(s) => {
            // Try to parse as UUID
            if let Ok(uuid) = uuid::Uuid::parse_str(&s) {
                Some(FilterValue::Uuid(uuid))
            } else {
                Some(FilterValue::String(s))
            }
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(FilterValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Some(FilterValue::Float(f))
            } else {
                None
            }
        }
        serde_json::Value::Bool(b) => Some(FilterValue::Boolean(b)),
        serde_json::Value::Array(arr) => {
            let values: Vec<FilterValue> =
                arr.into_iter().filter_map(json_to_filter_value).collect();
            if values.is_empty() {
                None
            } else {
                Some(FilterValue::List(values))
            }
        }
        _ => None,
    }
}

/// Render gather content as an HTML fragment (no page wrapper).
fn render_gather_content_html(view: &GatherView, result: &crate::gather::GatherResult) -> String {
    let mut html = String::new();

    // Title
    html.push_str(&format!("<h1>{}</h1>\n", escape_html(&view.label)));

    if let Some(ref desc) = view.description {
        html.push_str(&format!("<p>{}</p>\n", escape_html(desc)));
    }

    // Results
    if result.items.is_empty() {
        let empty_text = view
            .display
            .empty_text
            .as_deref()
            .unwrap_or("No results found.");
        html.push_str(&format!(
            "<p class=\"empty\">{}</p>\n",
            escape_html(empty_text)
        ));
    } else {
        html.push_str("<table class=\"table\">\n<thead>\n<tr>\n");

        if let Some(first) = result.items.first() {
            if let Some(obj) = first.as_object() {
                for key in obj.keys() {
                    html.push_str(&format!("<th>{}</th>\n", escape_html(key)));
                }
                html.push_str("</tr>\n</thead>\n<tbody>\n");

                for item in &result.items {
                    html.push_str("<tr>\n");
                    if let Some(obj) = item.as_object() {
                        for key in obj.keys() {
                            let value = obj
                                .get(key)
                                .map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    serde_json::Value::Null => "".to_string(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_default();
                            html.push_str(&format!("<td>{}</td>\n", escape_html(&value)));
                        }
                    }
                    html.push_str("</tr>\n");
                }
            }
        }

        html.push_str("</tbody>\n</table>\n");
    }

    // Pager
    if view.display.pager.enabled && result.total_pages > 1 {
        html.push_str("<div class=\"pager\">\n");

        if view.display.pager.show_count {
            html.push_str(&format!(
                "<span>Showing page {} of {} ({} total)</span>\n",
                result.page, result.total_pages, result.total
            ));
        }

        if result.has_prev {
            html.push_str(&format!(
                "<a href=\"?page={}\">Previous</a>\n",
                result.page - 1
            ));
        }

        if result.has_next {
            html.push_str(&format!("<a href=\"?page={}\">Next</a>\n", result.page + 1));
        }

        html.push_str("</div>\n");
    }

    html
}

/// Full standalone fallback page (used when theme engine fails).
fn render_gather_html(view: &GatherView, result: &crate::gather::GatherResult) -> String {
    let content = render_gather_content_html(view, result);
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<title>{}</title>\n\
        <style>\nbody {{ font-family: system-ui, sans-serif; max-width: 1200px; margin: 0 auto; padding: 20px; }}\n\
        table {{ width: 100%; border-collapse: collapse; }}\nth, td {{ padding: 8px; text-align: left; border-bottom: 1px solid #ddd; }}\n\
        th {{ background: #f5f5f5; }}\n.pager {{ margin-top: 20px; }}\n.pager a {{ margin: 0 5px; }}\n\
        .empty {{ color: #666; font-style: italic; }}\n</style>\n</head>\n<body>\n{}\n</body>\n</html>",
        escape_html(&view.label),
        content
    )
}
