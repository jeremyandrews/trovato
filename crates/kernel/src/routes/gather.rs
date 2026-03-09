//! Gather query API routes.
//!
//! REST endpoints for executing gather queries.

use crate::gather::{
    ExposedWidget, FilterValue, GatherQuery, QueryContext, QueryDefinition, QueryDisplay,
};
use crate::models::TagWithDepth;
use crate::models::stage::LIVE_STAGE_ID;
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
use std::collections::{HashMap, HashSet};
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
pub struct ExecuteParams {
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
    LIVE_STAGE_ID.to_string()
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
    let stage_id = params.stage.parse::<Uuid>().unwrap_or(LIVE_STAGE_ID);

    let result = state
        .gather()
        .execute(&query_id, params.page, exposed_filters, stage_id, &context)
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

    let stage_id = request.stage.parse::<Uuid>().unwrap_or(LIVE_STAGE_ID);

    let result = state
        .gather()
        .execute_definition(
            &request.definition,
            &request.display,
            request.page,
            exposed_filters,
            stage_id,
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
    // Use canonical_url from the query definition when available so that pager
    // links and filter form actions stay on the friendly URL (e.g. /conferences)
    // rather than /gather/{query_id}.
    let canonical = state
        .gather()
        .get_query(&query_id)
        .and_then(|q| q.display.canonical_url);
    let base_path = canonical.unwrap_or_else(|| format!("/gather/{query_id}"));
    execute_and_render(&state, &session, &query_id, params, &base_path).await
}

/// Execute a gather query and render it as an HTML page.
///
/// `base_path` controls the URL used for pager links and form actions.
pub async fn execute_and_render(
    state: &AppState,
    session: &Session,
    query_id: &str,
    params: ExecuteParams,
    base_path: &str,
) -> Result<Html<String>, (StatusCode, Json<JsonError>)> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let query_context = QueryContext {
        current_user_id: user_id,
        url_args: params.filters.clone(),
    };

    let gather_query = state.gather().get_query(query_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "query not found".to_string(),
            }),
        )
    })?;

    let exposed_filters = parse_filter_params(&params.filters);
    let stage_id = params.stage.parse::<Uuid>().unwrap_or(LIVE_STAGE_ID);

    // Pre-fetch widget data before executing — borrows exposed_filters for faceted
    // scoping, then releases the borrow so execute() can take ownership.
    let preload = preload_widget_data(state, &gather_query, &exposed_filters).await;

    let result = state
        .gather()
        .execute(
            query_id,
            params.page,
            exposed_filters,
            stage_id,
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
    let content_html = render_gather_with_theme(
        state,
        &gather_query,
        &result,
        &filter_values,
        base_path,
        &preload,
    )
    .unwrap_or_else(|| {
        render_gather_content_html(&gather_query, &result, &filter_values, base_path, &preload)
    });

    // Wrap in page layout with site context
    let mut context = tera::Context::new();
    super::helpers::inject_site_context(state, session, &mut context, base_path).await;

    // Build breadcrumbs: Home > Query Label
    let breadcrumbs = vec![
        serde_json::json!({"path": "/", "title": "Home"}),
        serde_json::json!({"path": null, "title": gather_query.label}),
    ];
    context.insert("breadcrumbs", &breadcrumbs);

    let page_html = state
        .theme()
        .render_page(base_path, &gather_query.label, &content_html, &mut context)
        .unwrap_or_else(|_| render_gather_html(&gather_query, &result));

    Ok(Html(page_html))
}

fn render_gather_with_theme(
    state: &AppState,
    query: &GatherQuery,
    result: &crate::gather::GatherResult,
    filter_values: &HashMap<String, String>,
    base_path: &str,
    preload: &WidgetPreloadData,
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
    context.insert("base_path", base_path);

    // Exposed filters for template rendering (includes widget type + options)
    let exposed_filters = collect_exposed_filters(query, preload);
    context.insert("exposed_filters", &exposed_filters);
    context.insert("filter_values", filter_values);

    // Pager info
    if query.display.pager.enabled && result.total_pages > 1 {
        let page_url_prefix = build_page_url_prefix(base_path, filter_values);
        context.insert(
            "pager",
            &serde_json::json!({
                "current_page": result.page,
                "total_pages": result.total_pages,
                "page_url_prefix": page_url_prefix,
                "pages": compute_page_list(result.page, result.total_pages),
            }),
        );
    }

    state
        .theme()
        .tera()
        .render(&template, &context)
        .inspect_err(
            |e| tracing::warn!(template = %template, error = ?e, "gather template render failed"),
        )
        .ok()
}

// -------------------------------------------------------------------------
// Widget preloading
// -------------------------------------------------------------------------

/// Pre-fetched data needed by exposed-filter widgets during rendering.
struct WidgetPreloadData {
    /// Tags (with depth) for `TaxonomySelect` widgets, keyed by vocabulary name.
    taxonomy_options: HashMap<String, Vec<TagWithDepth>>,
    /// Distinct values for `DynamicOptions` widgets, keyed by source field path.
    dynamic_options: HashMap<String, Vec<String>>,
}

/// Pre-fetch all widget data required by the exposed filters of `query`.
///
/// Uses `active_filters` to apply faceted scoping: each widget's option list
/// is restricted to values that produce at least one result when combined with
/// the other currently-active filter selections.
///
/// Called once per request before any rendering so that both the theme
/// template path and the fallback HTML path share the same data.
async fn preload_widget_data(
    state: &AppState,
    query: &GatherQuery,
    active_filters: &HashMap<String, FilterValue>,
) -> WidgetPreloadData {
    let item_type = query.definition.item_type.as_deref().unwrap_or("");
    let all_exposed = &query.definition.filters;
    let mut taxonomy_options: HashMap<String, Vec<TagWithDepth>> = HashMap::new();
    let mut dynamic_options: HashMap<String, Vec<String>> = HashMap::new();

    for filter in query.definition.filters.iter().filter(|f| f.exposed) {
        match &filter.widget {
            ExposedWidget::TaxonomySelect { vocabulary } => {
                if !taxonomy_options.contains_key(vocabulary) {
                    let all_tags = state
                        .categories()
                        .list_tags_with_depth(vocabulary)
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                vocabulary,
                                error = %e,
                                "failed to preload taxonomy options for widget"
                            );
                            Vec::new()
                        });
                    if all_tags.is_empty() {
                        tracing::warn!(
                            vocabulary,
                            "taxonomy_select widget: vocabulary not found or has no terms"
                        );
                    }
                    let reachable = state
                        .gather()
                        .fetch_faceted_reachable_tag_ids(
                            &filter.field,
                            item_type,
                            all_exposed,
                            &filter.field,
                            active_filters,
                        )
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                field = %filter.field,
                                error = %e,
                                "failed to fetch reachable tag IDs for faceted widget"
                            );
                            None
                        });
                    taxonomy_options.insert(
                        vocabulary.clone(),
                        filter_visible_tags(&all_tags, reachable),
                    );
                }
            }
            ExposedWidget::DynamicOptions { source_field, .. } => {
                if !dynamic_options.contains_key(source_field) {
                    let values = state
                        .gather()
                        .fetch_faceted_distinct_values(
                            source_field,
                            item_type,
                            all_exposed,
                            &filter.field,
                            active_filters,
                        )
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                source_field,
                                error = %e,
                                "failed to preload dynamic options for widget"
                            );
                            Vec::new()
                        });
                    dynamic_options.insert(source_field.clone(), values);
                }
            }
            ExposedWidget::Text | ExposedWidget::Boolean => {}
        }
    }

    WidgetPreloadData {
        taxonomy_options,
        dynamic_options,
    }
}

/// Filter a flat DFS-ordered taxonomy list to tags reachable given the current scope.
///
/// When `reachable` is `None` (no scope active), all tags are returned unchanged.
/// When `reachable` is `Some(set)`, returns tags that are either directly in
/// `reachable` or are an ancestor of a reachable tag (to preserve tree structure).
fn filter_visible_tags(
    all_tags: &[TagWithDepth],
    reachable: Option<HashSet<Uuid>>,
) -> Vec<TagWithDepth> {
    let Some(reachable) = reachable else {
        return all_tags.to_vec();
    };

    if reachable.is_empty() {
        return Vec::new();
    }

    let n = all_tags.len();
    let mut visible = vec![false; n];

    // Forward pass: mark directly reachable tags.
    for (i, twd) in all_tags.iter().enumerate() {
        if reachable.contains(&twd.tag.id) {
            visible[i] = true;
        }
    }

    // Backward pass: for each visible tag with depth > 0, mark its direct parent
    // (the last preceding tag whose depth is exactly one less).
    for i in (0..n).rev() {
        if visible[i] && all_tags[i].depth > 0 {
            let parent_depth = all_tags[i].depth - 1;
            for j in (0..i).rev() {
                if all_tags[j].depth == parent_depth {
                    visible[j] = true;
                    break;
                }
            }
        }
    }

    all_tags
        .iter()
        .zip(visible.iter())
        .filter(|(_, v)| **v)
        .map(|(t, _)| t.clone())
        .collect()
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn parse_filter_params(params: &HashMap<String, String>) -> HashMap<String, FilterValue> {
    params
        .iter()
        .filter(|(k, _)| !["page", "stage"].contains(&k.as_str()))
        .filter(|(_, v)| !v.is_empty())
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

/// Build the URL prefix used for pager links, preserving active filter params.
///
/// Returns `/gather/{query_id}?{encoded_filters}&` when filters are present,
/// or `/gather/{query_id}?` when no filters are active. Callers append `page=N`.
fn build_page_url_prefix(base_path: &str, filter_values: &HashMap<String, String>) -> String {
    let active: Vec<String> = filter_values
        .iter()
        .filter(|(k, v)| !["page", "stage"].contains(&k.as_str()) && !v.is_empty())
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect();

    let joined = active.join("&");
    if active.is_empty() {
        format!("{base_path}?")
    } else {
        format!("{base_path}?{joined}&")
    }
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

/// Collect exposed filter metadata from a query definition, including widget
/// type information and pre-fetched option lists for select/autocomplete widgets.
fn collect_exposed_filters(
    query: &GatherQuery,
    preload: &WidgetPreloadData,
) -> Vec<serde_json::Value> {
    query
        .definition
        .filters
        .iter()
        .filter(|f| f.exposed)
        .map(|f| {
            let label = f.exposed_label.as_deref().unwrap_or(&f.field);
            match &f.widget {
                ExposedWidget::Boolean => serde_json::json!({
                    "field": f.field,
                    "label": label,
                    "widget_type": "boolean",
                }),
                ExposedWidget::TaxonomySelect { vocabulary } => {
                    let options: Vec<serde_json::Value> = preload
                        .taxonomy_options
                        .get(vocabulary)
                        .map(|tags| {
                            tags.iter()
                                .map(|twd| {
                                    serde_json::json!({
                                        "value": twd.tag.id,
                                        "label": twd.tag.label,
                                        "depth": twd.depth,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    serde_json::json!({
                        "field": f.field,
                        "label": label,
                        "widget_type": "taxonomy_select",
                        "options": options,
                    })
                }
                ExposedWidget::DynamicOptions {
                    source_field,
                    autocomplete_threshold,
                } => {
                    let values = preload
                        .dynamic_options
                        .get(source_field)
                        .cloned()
                        .unwrap_or_default();
                    let is_autocomplete = values.len() > *autocomplete_threshold;
                    serde_json::json!({
                        "field": f.field,
                        "label": label,
                        "widget_type": "dynamic_options",
                        "options": values,
                        "is_autocomplete": is_autocomplete,
                    })
                }
                ExposedWidget::Text => serde_json::json!({
                    "field": f.field,
                    "label": label,
                    "widget_type": "text",
                }),
            }
        })
        .collect()
}

/// Render exposed filter form as HTML (fallback path, no theme template).
///
/// Renders appropriate controls for each widget type:
/// - `Boolean` → `<select>` with Any / Yes / No
/// - `TaxonomySelect` → `<select>` indented by hierarchy depth
/// - `DynamicOptions` → `<select>` (≤ threshold) or `<datalist>` autocomplete (> threshold)
/// - `Text` → plain `<input type="text">`
fn render_exposed_filter_form(
    query: &GatherQuery,
    filter_values: &HashMap<String, String>,
    base_path: &str,
    preload: &WidgetPreloadData,
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

    let mut html = format!(
        "<form method=\"get\" action=\"{}\" class=\"gather-exposed-filters\">\n",
        escape_html(base_path)
    );

    for filter in &exposed {
        let label = filter.exposed_label.as_deref().unwrap_or(&filter.field);
        let field_escaped = escape_html(&filter.field);
        let label_escaped = escape_html(label);
        let current = filter_values
            .get(&filter.field)
            .map(String::as_str)
            .unwrap_or("");

        html.push_str(&format!(
            "<div class=\"form-group form-group--inline\">\
             <label for=\"filter-{field_escaped}\">{label_escaped}</label>\n"
        ));

        match &filter.widget {
            ExposedWidget::Boolean => {
                let sel_true = if current == "true" { " selected" } else { "" };
                let sel_false = if current == "false" { " selected" } else { "" };
                html.push_str(&format!(
                    "<select id=\"filter-{field_escaped}\" name=\"{field_escaped}\" \
                     class=\"form-control\">\n\
                     <option value=\"\">Any</option>\n\
                     <option value=\"true\"{sel_true}>Yes</option>\n\
                     <option value=\"false\"{sel_false}>No</option>\n\
                     </select>\n"
                ));
            }
            ExposedWidget::TaxonomySelect { vocabulary } => {
                html.push_str(&format!(
                    "<select id=\"filter-{field_escaped}\" name=\"{field_escaped}\" \
                     class=\"form-control\">\n\
                     <option value=\"\">Any</option>\n"
                ));

                let tags = preload
                    .taxonomy_options
                    .get(vocabulary)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);

                for twd in tags {
                    let indent = "\u{00a0}\u{00a0}".repeat(twd.depth as usize);
                    let tag_id = twd.tag.id.to_string();
                    let selected = if current == tag_id { " selected" } else { "" };
                    let label_text = escape_html(&twd.tag.label);
                    html.push_str(&format!(
                        "<option value=\"{id}\"{selected}>{indent}{label_text}</option>\n",
                        id = escape_html(&tag_id),
                    ));
                }
                html.push_str("</select>\n");
            }
            ExposedWidget::DynamicOptions {
                source_field,
                autocomplete_threshold,
            } => {
                let values = preload
                    .dynamic_options
                    .get(source_field)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);

                if values.len() > *autocomplete_threshold {
                    // Autocomplete: text input + datalist
                    html.push_str(&format!(
                        "<input type=\"text\" id=\"filter-{field_escaped}\" \
                         name=\"{field_escaped}\" value=\"{value}\" \
                         class=\"form-control\" list=\"{field_escaped}-options\">\n\
                         <datalist id=\"{field_escaped}-options\">\n",
                        value = escape_html(current),
                    ));
                    for v in values {
                        html.push_str(&format!("<option value=\"{}\"></option>\n", escape_html(v)));
                    }
                    html.push_str("</datalist>\n");
                } else {
                    // Dropdown select
                    html.push_str(&format!(
                        "<select id=\"filter-{field_escaped}\" name=\"{field_escaped}\" \
                         class=\"form-control\">\n\
                         <option value=\"\">Any</option>\n"
                    ));
                    for v in values {
                        let selected = if current == v { " selected" } else { "" };
                        let v_esc = escape_html(v);
                        html.push_str(&format!(
                            "<option value=\"{v_esc}\"{selected}>{v_esc}</option>\n"
                        ));
                    }
                    html.push_str("</select>\n");
                }
            }
            ExposedWidget::Text => {
                html.push_str(&format!(
                    "<input type=\"text\" id=\"filter-{field_escaped}\" \
                     name=\"{field_escaped}\" value=\"{value}\" class=\"form-control\">\n",
                    value = escape_html(current),
                ));
            }
        }

        html.push_str("</div>\n");
    }

    html.push_str(&format!(
        "<div class=\"form-actions\">\
         <button type=\"submit\" class=\"btn\">Apply</button>\
         <a href=\"{}\" class=\"btn btn--secondary\">Clear</a>\
         </div>\n</form>\n",
        escape_html(base_path)
    ));

    html
}

/// Render gather content as an HTML fragment (no page wrapper).
fn render_gather_content_html(
    query: &GatherQuery,
    result: &crate::gather::GatherResult,
    filter_values: &HashMap<String, String>,
    base_path: &str,
    preload: &WidgetPreloadData,
) -> String {
    let mut html = String::new();

    // Title
    html.push_str(&format!("<h1>{}</h1>\n", escape_html(&query.label)));

    if let Some(ref desc) = query.description {
        html.push_str(&format!("<p>{}</p>\n", escape_html(desc)));
    }

    // Exposed filter form
    html.push_str(&render_exposed_filter_form(
        query,
        filter_values,
        base_path,
        preload,
    ));

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

        let prefix = build_page_url_prefix(base_path, filter_values);
        if result.has_prev {
            html.push_str(&format!(
                "<a href=\"{}page={}\">Previous</a>\n",
                prefix,
                result.page - 1
            ));
        }

        if result.has_next {
            html.push_str(&format!(
                "<a href=\"{}page={}\">Next</a>\n",
                prefix,
                result.page + 1
            ));
        }

        html.push_str("</div>\n");
    }

    html
}

/// Build a list of page numbers (and `"..."` ellipsis sentinels) for pager templates.
///
/// Always includes page 1, the last page, and a window of 2 pages either side
/// of the current page.  Gaps are filled with the string `"..."`.
/// Returns an empty list when `total == 0`.
fn compute_page_list(current: u32, total: u32) -> Vec<serde_json::Value> {
    if total == 0 {
        return vec![];
    }

    let window = 2u32;
    let mut shown = std::collections::BTreeSet::new();
    shown.insert(1u32);
    shown.insert(total);
    // Clamp lower bound to 1 so page 0 is never inserted when current < window.
    let lo = current.saturating_sub(window).max(1);
    let hi = std::cmp::min(current + window, total);
    for p in lo..=hi {
        shown.insert(p);
    }

    let mut pages: Vec<serde_json::Value> = Vec::new();
    let mut last = 0u32;
    for &p in &shown {
        if last > 0 && p > last + 1 {
            pages.push(serde_json::json!("..."));
        }
        pages.push(serde_json::json!(p));
        last = p;
    }
    pages
}

/// Full standalone fallback page (used when theme engine fails).
fn render_gather_html(query: &GatherQuery, result: &crate::gather::GatherResult) -> String {
    let base_path = format!("/gather/{}", query.query_id);
    let empty_preload = WidgetPreloadData {
        taxonomy_options: HashMap::new(),
        dynamic_options: HashMap::new(),
    };
    let content =
        render_gather_content_html(query, result, &HashMap::new(), &base_path, &empty_preload);
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod pager_tests {
    use super::compute_page_list;

    fn nums(list: &[serde_json::Value]) -> Vec<String> {
        list.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn empty_when_total_zero() {
        assert!(compute_page_list(0, 0).is_empty());
        assert!(compute_page_list(1, 0).is_empty());
    }

    #[test]
    fn single_page() {
        assert_eq!(nums(&compute_page_list(1, 1)), vec!["1"]);
    }

    #[test]
    fn no_page_zero_near_start() {
        // current=1, window=2 would produce 0 without the max(1) clamp
        let pages = compute_page_list(1, 10);
        let first = pages.first().unwrap().as_u64();
        assert_eq!(first, Some(1), "first page must be 1, got {pages:?}");
    }

    #[test]
    fn ellipsis_in_middle() {
        // pages 1..10, current=5 → [1, "...", 3, 4, 5, 6, 7, "...", 10]
        let pages = compute_page_list(5, 10);
        let s = nums(&pages);
        assert!(
            s.contains(&"\"...\"".to_string()),
            "expected ellipsis: {s:?}"
        );
        assert!(s.contains(&"1".to_string()));
        assert!(s.contains(&"10".to_string()));
        assert!(s.contains(&"5".to_string()));
    }

    #[test]
    fn no_ellipsis_when_contiguous() {
        // pages 1..5, current=3 → [1,2,3,4,5] no gaps
        let pages = compute_page_list(3, 5);
        let s = nums(&pages);
        assert!(
            !s.contains(&"\"...\"".to_string()),
            "unexpected ellipsis: {s:?}"
        );
        assert_eq!(s, vec!["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn near_end_no_page_beyond_total() {
        let pages = compute_page_list(10, 10);
        let max = pages.iter().filter_map(|v| v.as_u64()).max().unwrap_or(0);
        assert_eq!(max, 10);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Widget render tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod widget_tests {
    use super::{WidgetPreloadData, collect_exposed_filters, render_exposed_filter_form};
    use crate::gather::{
        ExposedWidget, FilterOperator, FilterValue, GatherQuery, QueryDefinition, QueryDisplay,
        QueryFilter,
    };
    use crate::models::{Tag, TagWithDepth};
    use std::collections::HashMap;
    use uuid::Uuid;

    // ── Helpers ───────────────────────────────────────────────────────

    fn make_query(filters: Vec<QueryFilter>) -> GatherQuery {
        GatherQuery {
            query_id: "test_q".to_string(),
            label: "Test Query".to_string(),
            description: None,
            definition: QueryDefinition {
                filters,
                ..Default::default()
            },
            display: QueryDisplay::default(),
            plugin: "test".to_string(),
            created: 0,
            changed: 0,
        }
    }

    fn make_filter(field: &str, widget: ExposedWidget, label: &str) -> QueryFilter {
        QueryFilter {
            field: field.to_string(),
            operator: FilterOperator::Equals,
            value: FilterValue::Null(()),
            exposed: true,
            exposed_label: Some(label.to_string()),
            widget,
        }
    }

    fn make_tag(label: &str, depth: i32) -> TagWithDepth {
        TagWithDepth {
            tag: Tag {
                id: Uuid::nil(),
                category_id: "topic".to_string(),
                label: label.to_string(),
                description: None,
                slug: None,
                weight: 0,
                created: 0,
                changed: 0,
            },
            depth,
        }
    }

    fn empty_preload() -> WidgetPreloadData {
        WidgetPreloadData {
            taxonomy_options: HashMap::new(),
            dynamic_options: HashMap::new(),
        }
    }

    // ── Boolean widget ────────────────────────────────────────────────

    #[test]
    fn boolean_widget_renders_select_with_three_options() {
        let query = make_query(vec![make_filter(
            "field_online",
            ExposedWidget::Boolean,
            "Online Only",
        )]);
        let html =
            render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &empty_preload());
        assert!(html.contains("<select"), "expected <select>: {html}");
        assert!(html.contains(">Any<"), "expected Any option: {html}");
        assert!(html.contains(">Yes<"), "expected Yes option: {html}");
        assert!(html.contains(">No<"), "expected No option: {html}");
        // No option should be pre-selected when value is empty
        assert!(!html.contains("selected"), "unexpected selected: {html}");
    }

    #[test]
    fn boolean_widget_preselects_yes_when_value_is_true() {
        let query = make_query(vec![make_filter(
            "field_online",
            ExposedWidget::Boolean,
            "Online Only",
        )]);
        let mut values = HashMap::new();
        values.insert("field_online".to_string(), "true".to_string());

        let html = render_exposed_filter_form(&query, &values, "/gather/test_q", &empty_preload());
        assert!(
            html.contains("value=\"true\" selected"),
            "expected Yes selected: {html}"
        );
        assert!(
            !html.contains("value=\"false\" selected"),
            "false should not be selected: {html}"
        );
    }

    #[test]
    fn boolean_widget_preselects_no_when_value_is_false() {
        let query = make_query(vec![make_filter(
            "field_online",
            ExposedWidget::Boolean,
            "Online Only",
        )]);
        let mut values = HashMap::new();
        values.insert("field_online".to_string(), "false".to_string());

        let html = render_exposed_filter_form(&query, &values, "/gather/test_q", &empty_preload());
        assert!(
            html.contains("value=\"false\" selected"),
            "expected No selected: {html}"
        );
    }

    // ── TaxonomySelect widget ─────────────────────────────────────────

    #[test]
    fn taxonomy_select_renders_options_from_preload() {
        let query = make_query(vec![make_filter(
            "field_topics",
            ExposedWidget::TaxonomySelect {
                vocabulary: "topic".to_string(),
            },
            "Topic",
        )]);
        let mut preload = empty_preload();
        preload.taxonomy_options.insert(
            "topic".to_string(),
            vec![make_tag("Science", 0), make_tag("Physics", 1)],
        );

        let html = render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &preload);
        assert!(html.contains("<select"), "expected <select>: {html}");
        assert!(html.contains(">Any<"), "expected Any option: {html}");
        assert!(html.contains("Science"), "expected Science: {html}");
        assert!(html.contains("Physics"), "expected Physics: {html}");
    }

    #[test]
    fn taxonomy_select_indents_child_terms() {
        let query = make_query(vec![make_filter(
            "field_topics",
            ExposedWidget::TaxonomySelect {
                vocabulary: "topic".to_string(),
            },
            "Topic",
        )]);
        let mut preload = empty_preload();
        preload.taxonomy_options.insert(
            "topic".to_string(),
            vec![make_tag("Root", 0), make_tag("Child", 1)],
        );

        let html = render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &preload);

        // Depth-0 term must have no leading non-breaking spaces.
        let nbsp2 = "\u{00a0}\u{00a0}";
        assert!(
            !html.contains(&format!("{nbsp2}Root")),
            "depth-0 term should not be indented: {html}"
        );

        // Depth-1 term must be prefixed with exactly 2 non-breaking spaces,
        // NOT 4 (which would indicate the double-indent bug).
        let nbsp4 = "\u{00a0}\u{00a0}\u{00a0}\u{00a0}";
        assert!(
            html.contains(&format!("{nbsp2}Child")),
            "depth-1 term should have 2-nbsp indent: {html}"
        );
        assert!(
            !html.contains(&format!("{nbsp4}Child")),
            "depth-1 term must not have 4-nbsp (double-indent bug): {html}"
        );
    }

    #[test]
    fn taxonomy_select_renders_any_when_preload_empty() {
        let query = make_query(vec![make_filter(
            "field_topics",
            ExposedWidget::TaxonomySelect {
                vocabulary: "topic".to_string(),
            },
            "Topic",
        )]);
        // No entries in preload — vocabulary not loaded
        let html =
            render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &empty_preload());
        assert!(html.contains("<select"), "expected <select>: {html}");
        assert!(html.contains(">Any<"), "expected Any option: {html}");
    }

    // ── DynamicOptions widget ─────────────────────────────────────────

    #[test]
    fn dynamic_options_renders_select_below_threshold() {
        let query = make_query(vec![make_filter(
            "field_country",
            ExposedWidget::DynamicOptions {
                source_field: "fields.field_country".to_string(),
                autocomplete_threshold: 30,
            },
            "Country",
        )]);
        let mut preload = empty_preload();
        // 3 values — well below threshold of 30
        preload.dynamic_options.insert(
            "fields.field_country".to_string(),
            vec![
                "Germany".to_string(),
                "Spain".to_string(),
                "USA".to_string(),
            ],
        );

        let html = render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &preload);
        assert!(html.contains("<select"), "expected <select>: {html}");
        assert!(!html.contains("<datalist"), "unexpected <datalist>: {html}");
        assert!(html.contains("Germany"), "expected Germany: {html}");
    }

    #[test]
    fn dynamic_options_renders_datalist_above_threshold() {
        let values: Vec<String> = (1..=31).map(|i| format!("Country{i}")).collect();

        let query = make_query(vec![make_filter(
            "field_country",
            ExposedWidget::DynamicOptions {
                source_field: "fields.field_country".to_string(),
                autocomplete_threshold: 30,
            },
            "Country",
        )]);
        let mut preload = empty_preload();
        preload
            .dynamic_options
            .insert("fields.field_country".to_string(), values);

        let html = render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &preload);
        assert!(html.contains("<datalist"), "expected <datalist>: {html}");
        assert!(
            html.contains("Country1"),
            "expected option Country1: {html}"
        );
    }

    #[test]
    fn dynamic_options_preselects_current_value_in_select() {
        let query = make_query(vec![make_filter(
            "field_country",
            ExposedWidget::DynamicOptions {
                source_field: "fields.field_country".to_string(),
                autocomplete_threshold: 30,
            },
            "Country",
        )]);
        let mut preload = empty_preload();
        preload.dynamic_options.insert(
            "fields.field_country".to_string(),
            vec!["Germany".to_string(), "Spain".to_string()],
        );
        let mut values = HashMap::new();
        values.insert("field_country".to_string(), "Spain".to_string());

        let html = render_exposed_filter_form(&query, &values, "/gather/test_q", &preload);
        assert!(
            html.contains("value=\"Spain\" selected"),
            "expected Spain selected: {html}"
        );
        assert!(
            !html.contains("value=\"Germany\" selected"),
            "Germany should not be selected: {html}"
        );
    }

    // ── Text widget (default) ─────────────────────────────────────────

    #[test]
    fn text_widget_renders_input() {
        let query = make_query(vec![make_filter("title", ExposedWidget::Text, "Title")]);
        let html =
            render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &empty_preload());
        assert!(
            html.contains("type=\"text\""),
            "expected text input: {html}"
        );
        assert!(!html.contains("<select"), "unexpected <select>: {html}");
    }

    // ── collect_exposed_filters ───────────────────────────────────────

    #[test]
    fn collect_exposed_filters_returns_widget_type() {
        let query = make_query(vec![
            make_filter("field_online", ExposedWidget::Boolean, "Online Only"),
            make_filter(
                "field_topics",
                ExposedWidget::TaxonomySelect {
                    vocabulary: "topic".to_string(),
                },
                "Topic",
            ),
            make_filter("title", ExposedWidget::Text, "Title"),
        ]);

        let filters = collect_exposed_filters(&query, &empty_preload());
        assert_eq!(filters.len(), 3);

        assert_eq!(filters[0]["widget_type"], "boolean");
        assert_eq!(filters[0]["field"], "field_online");
        assert_eq!(filters[0]["label"], "Online Only");

        assert_eq!(filters[1]["widget_type"], "taxonomy_select");
        assert_eq!(filters[2]["widget_type"], "text");
    }

    #[test]
    fn collect_exposed_filters_skips_non_exposed() {
        let mut filter = make_filter("hidden", ExposedWidget::Boolean, "Hidden");
        filter.exposed = false;
        let query = make_query(vec![filter]);

        let filters = collect_exposed_filters(&query, &empty_preload());
        assert!(filters.is_empty());
    }

    #[test]
    fn collect_exposed_filters_dynamic_options_includes_is_autocomplete() {
        let query = make_query(vec![make_filter(
            "field_country",
            ExposedWidget::DynamicOptions {
                source_field: "fields.field_country".to_string(),
                autocomplete_threshold: 2,
            },
            "Country",
        )]);
        let mut preload = empty_preload();
        // 3 values > threshold of 2 → autocomplete mode
        preload.dynamic_options.insert(
            "fields.field_country".to_string(),
            vec!["A".to_string(), "B".to_string(), "C".to_string()],
        );

        let filters = collect_exposed_filters(&query, &preload);
        assert_eq!(filters[0]["widget_type"], "dynamic_options");
        assert_eq!(filters[0]["is_autocomplete"], true);
    }

    #[test]
    fn render_form_empty_when_no_exposed_filters() {
        // A query with only non-exposed filters should produce an empty string
        let mut filter = make_filter("status", ExposedWidget::Text, "Status");
        filter.exposed = false;
        let query = make_query(vec![filter]);

        let html =
            render_exposed_filter_form(&query, &HashMap::new(), "/gather/test_q", &empty_preload());
        assert!(html.is_empty(), "expected empty string, got: {html}");
    }
}
