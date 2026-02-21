//! Gather query API routes.
//!
//! REST endpoints for executing gather queries.

use crate::gather::{FilterValue, GatherQuery, QueryContext, QueryDefinition, QueryDisplay};
use crate::routes::auth::SESSION_USER_ID;
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
use tower_sessions::Session;
use uuid::Uuid;

use super::helpers::{JsonError, html_escape as escape_html};

/// Create the gather router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/queries", get(list_queries))
        .route("/api/query/{query_id}", get(get_query))
        .route("/api/query/{query_id}/execute", get(execute_query))
        .route("/api/gather/query", post(execute_adhoc_query))
        .route("/gather/{query_id}", get(render_query_html))
}

// -------------------------------------------------------------------------
// Response types
// -------------------------------------------------------------------------

#[derive(Serialize)]
struct QuerySummary {
    query_id: String,
    label: String,
    description: Option<String>,
    plugin: String,
}

#[derive(Serialize)]
struct QueryResponse {
    query_id: String,
    label: String,
    description: Option<String>,
    definition: QueryDefinition,
    display: QueryDisplay,
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
    definition: QueryDefinition,
    display: QueryDisplay,
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

async fn list_queries(
    State(state): State<AppState>,
) -> Result<Json<Vec<QuerySummary>>, (StatusCode, Json<JsonError>)> {
    let queries = state.gather().list_queries();

    Ok(Json(
        queries
            .into_iter()
            .map(|q| QuerySummary {
                query_id: q.query_id,
                label: q.label,
                description: q.description,
                plugin: q.plugin,
            })
            .collect(),
    ))
}

async fn get_query(
    State(state): State<AppState>,
    Path(query_id): Path<String>,
) -> Result<Json<QueryResponse>, (StatusCode, Json<JsonError>)> {
    let query = state.gather().get_query(&query_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "query not found".to_string(),
            }),
        )
    })?;

    Ok(Json(QueryResponse {
        query_id: query.query_id,
        label: query.label,
        description: query.description,
        definition: query.definition,
        display: query.display,
        plugin: query.plugin,
        created: query.created,
        changed: query.changed,
    }))
}

async fn execute_query(
    State(state): State<AppState>,
    session: Session,
    Path(query_id): Path<String>,
    Query(params): Query<ExecuteParams>,
) -> Result<Json<GatherResultResponse>, (StatusCode, Json<JsonError>)> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let context = QueryContext {
        current_user_id: user_id,
        url_args: params.filters.clone(),
    };

    // Parse exposed filter values
    let exposed_filters = parse_filter_params(&params.filters);

    let result = state
        .gather()
        .execute(
            &query_id,
            params.page,
            exposed_filters,
            &params.stage,
            &context,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
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
    session: Session,
    Json(request): Json<AdhocQueryRequest>,
) -> Result<Json<GatherResultResponse>, (StatusCode, Json<JsonError>)> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let context = QueryContext {
        current_user_id: user_id,
        url_args: HashMap::new(),
    };

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
            &context,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
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

async fn render_query_html(
    State(state): State<AppState>,
    session: Session,
    Path(query_id): Path<String>,
    Query(params): Query<ExecuteParams>,
) -> Result<Html<String>, (StatusCode, Json<JsonError>)> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let query_context = QueryContext {
        current_user_id: user_id,
        url_args: params.filters.clone(),
    };

    let gather_query = state.gather().get_query(&query_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "query not found".to_string(),
            }),
        )
    })?;

    let exposed_filters = parse_filter_params(&params.filters);

    let result = state
        .gather()
        .execute(
            &query_id,
            params.page,
            exposed_filters,
            &params.stage,
            &query_context,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: e.to_string(),
                }),
            )
        })?;

    // Collect filter values for template context
    let filter_values: HashMap<String, String> = params
        .filters
        .iter()
        .filter(|(k, _)| !["page", "stage"].contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Render gather content (either via theme template or fallback HTML)
    let content_html = render_gather_with_theme(&state, &gather_query, &result, &filter_values)
        .unwrap_or_else(|| render_gather_content_html(&gather_query, &result, &filter_values));

    // Wrap in page layout with site context
    let gather_path = format!("/gather/{query_id}");
    let mut context = tera::Context::new();
    super::helpers::inject_site_context(&state, &session, &mut context, &gather_path).await;

    let page_html = state
        .theme()
        .render_page(
            &gather_path,
            &gather_query.label,
            &content_html,
            &mut context,
        )
        .unwrap_or_else(|_| render_gather_html(&gather_query, &result));

    Ok(Html(page_html))
}

fn render_gather_with_theme(
    state: &AppState,
    query: &GatherQuery,
    result: &crate::gather::GatherResult,
    filter_values: &HashMap<String, String>,
) -> Option<String> {
    // Try to find a template for this query
    let suggestions = [
        format!("gather/query--{}.html", query.query_id),
        format!("gather/query--{}.html", query.display.format.as_str()),
        "gather/query.html".to_string(),
    ];

    let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();
    let template = state.theme().resolve_template(&suggestion_refs)?;

    // Build context
    let mut context = tera::Context::new();
    context.insert("query", query);
    context.insert("rows", &result.items);
    context.insert("total", &result.total);
    context.insert("page", &result.page);
    context.insert("per_page", &result.per_page);
    context.insert("total_pages", &result.total_pages);
    context.insert("has_next", &result.has_next);
    context.insert("has_prev", &result.has_prev);

    // Exposed filters for template rendering
    let exposed_filters = collect_exposed_filters(query);
    context.insert("exposed_filters", &exposed_filters);
    context.insert("filter_values", filter_values);

    // Pager info
    if query.display.pager.enabled && result.total_pages > 1 {
        context.insert(
            "pager",
            &serde_json::json!({
                "current_page": result.page,
                "total_pages": result.total_pages,
                "base_url": format!("/gather/{}", query.query_id),
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
            } else {
                n.as_f64().map(FilterValue::Float)
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

/// Collect exposed filter metadata from a query definition.
fn collect_exposed_filters(query: &GatherQuery) -> Vec<serde_json::Value> {
    query
        .definition
        .filters
        .iter()
        .filter(|f| f.exposed)
        .map(|f| {
            serde_json::json!({
                "field": f.field,
                "label": f.exposed_label.as_deref().unwrap_or(&f.field),
                "operator": format!("{:?}", f.operator),
            })
        })
        .collect()
}

/// Render exposed filter form as HTML.
fn render_exposed_filter_form(
    query: &GatherQuery,
    filter_values: &HashMap<String, String>,
) -> String {
    let exposed: Vec<_> = query
        .definition
        .filters
        .iter()
        .filter(|f| f.exposed)
        .collect();

    if exposed.is_empty() {
        return String::new();
    }

    let mut html = String::new();
    html.push_str(&format!(
        "<form method=\"get\" action=\"/gather/{}\" class=\"gather-exposed-filters\">\n",
        escape_html(&query.query_id)
    ));

    for filter in &exposed {
        let label = filter.exposed_label.as_deref().unwrap_or(&filter.field);
        let value = filter_values
            .get(&filter.field)
            .map(|v| escape_html(v))
            .unwrap_or_default();
        html.push_str(&format!(
            "<div class=\"form-group form-group--inline\">\
             <label for=\"filter-{field}\">{label}</label>\
             <input type=\"text\" id=\"filter-{field}\" name=\"{field}\" value=\"{value}\" class=\"form-control\">\
             </div>\n",
            field = escape_html(&filter.field),
            label = escape_html(label),
            value = value,
        ));
    }

    html.push_str("<div class=\"form-actions\"><button type=\"submit\" class=\"btn\">Apply</button></div>\n</form>\n");
    html
}

/// Render gather content as an HTML fragment (no page wrapper).
fn render_gather_content_html(
    query: &GatherQuery,
    result: &crate::gather::GatherResult,
    filter_values: &HashMap<String, String>,
) -> String {
    let mut html = String::new();

    // Title
    html.push_str(&format!("<h1>{}</h1>\n", escape_html(&query.label)));

    if let Some(ref desc) = query.description {
        html.push_str(&format!("<p>{}</p>\n", escape_html(desc)));
    }

    // Exposed filter form
    html.push_str(&render_exposed_filter_form(query, filter_values));

    // Results
    if result.items.is_empty() {
        let empty_text = query
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

        if let Some(first) = result.items.first()
            && let Some(obj) = first.as_object()
        {
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

        html.push_str("</tbody>\n</table>\n");
    }

    // Pager
    if query.display.pager.enabled && result.total_pages > 1 {
        html.push_str("<div class=\"pager\">\n");

        if query.display.pager.show_count {
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
fn render_gather_html(query: &GatherQuery, result: &crate::gather::GatherResult) -> String {
    let content = render_gather_content_html(query, result, &HashMap::new());
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<title>{}</title>\n\
        <style>\nbody {{ font-family: system-ui, sans-serif; max-width: 1200px; margin: 0 auto; padding: 20px; }}\n\
        table {{ width: 100%; border-collapse: collapse; }}\nth, td {{ padding: 8px; text-align: left; border-bottom: 1px solid #ddd; }}\n\
        th {{ background: #f5f5f5; }}\n.pager {{ margin-top: 20px; }}\n.pager a {{ margin: 0 5px; }}\n\
        .empty {{ color: #666; font-style: italic; }}\n</style>\n</head>\n<body>\n{}\n</body>\n</html>",
        escape_html(&query.label),
        content
    )
}
