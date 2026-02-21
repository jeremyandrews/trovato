//! Item CRUD route handlers.
//!
//! Provides endpoints for viewing, creating, editing, and deleting content items.

use axum::{
    Extension, Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::{FilterPipeline, FormBuilder};
use crate::form::csrf::generate_csrf_token;
use crate::models::{CreateItem, UpdateItem, UrlAlias, User};
use crate::state::AppState;
use crate::tap::UserContext;

use super::auth::SESSION_USER_ID;
use super::helpers::{CsrfOnlyForm, JsonError, html_escape};

/// Response for successful item operations.
#[derive(Debug, Serialize)]
pub struct ItemResponse {
    pub id: Uuid,
    pub title: String,
    pub item_type: String,
    pub status: i16,
}

/// Full item response for JSON API.
#[derive(Debug, Serialize)]
pub struct ItemApiResponse {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub item_type: String,
    pub title: String,
    pub status: i16,
    pub author_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<AuthorResponse>,
    pub created: i64,
    pub changed: i64,
    pub promote: i16,
    pub sticky: i16,
    pub fields: serde_json::Value,
    pub stage_id: String,
}

/// Author information for embedding.
#[derive(Debug, Clone, Serialize)]
pub struct AuthorResponse {
    pub id: Uuid,
    pub name: String,
}

/// Paginated list response.
#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub pagination: PaginationMeta,
}

/// Pagination metadata.
#[derive(Debug, Serialize)]
pub struct PaginationMeta {
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

/// Query parameters for listing items.
#[derive(Debug, Deserialize)]
pub struct ListItemsQuery {
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    pub status: Option<i16>,
    pub author_id: Option<Uuid>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub include: Option<String>,
}

/// Query parameters for getting a single item.
#[derive(Debug, Deserialize)]
pub struct GetItemQuery {
    pub include: Option<String>,
}

/// Request for creating an item.
#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub title: String,
    pub status: Option<i16>,
    pub fields: Option<serde_json::Value>,
    pub log: Option<String>,
}

/// Request for updating an item.
#[derive(Debug, Deserialize)]
pub struct UpdateItemRequest {
    pub title: Option<String>,
    pub status: Option<i16>,
    pub fields: Option<serde_json::Value>,
    pub log: Option<String>,
    pub url_alias: Option<String>,
}

/// Create the item router.
pub fn router() -> Router<AppState> {
    Router::new()
        // View item
        .route("/item/{id}", get(view_item))
        // Add item form and submission
        .route("/item/add/{type}", get(add_item_form))
        .route("/item/add/{type}", post(create_item))
        // Edit item form and submission
        .route("/item/{id}/edit", get(edit_item_form))
        .route("/item/{id}/edit", post(update_item))
        // Delete item
        .route("/item/{id}/delete", post(delete_item))
        // Revision history
        .route("/item/{id}/revisions", get(list_revisions))
        .route("/item/{id}/revert/{rev_id}", post(revert_revision))
        // API endpoints
        .route("/api/content-types", get(list_content_types))
        .route("/api/items/{type}", get(list_items_by_type))
        // JSON API endpoints
        .route("/api/item/{id}", get(get_item_api))
        .route("/api/items", get(list_items_api))
}

/// Get current user from session.
async fn get_user_context(session: &Session, _state: &AppState) -> UserContext {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    match user_id {
        Some(id) => {
            // Load user permissions
            // TODO: Implement actual permission loading
            UserContext {
                id,
                authenticated: true,
                permissions: vec![
                    "access content".to_string(),
                    "create page content".to_string(),
                    "edit own page content".to_string(),
                    "delete own page content".to_string(),
                ],
            }
        }
        None => UserContext::anonymous(),
    }
}

/// Determine which text formats the user is allowed to use.
///
/// Admins get all formats. Other users get formats based on their
/// `"use filtered_html"` and `"use full_html"` permissions.
/// `plain_text` is always allowed (handled by FormBuilder).
fn permitted_text_formats(user: &UserContext) -> Vec<String> {
    if user.is_admin() {
        return vec![
            "plain_text".to_string(),
            "filtered_html".to_string(),
            "full_html".to_string(),
        ];
    }

    let mut formats = vec!["plain_text".to_string()];
    if user.has_permission("use filtered_html") {
        formats.push("filtered_html".to_string());
    }
    if user.has_permission("use full_html") {
        formats.push("full_html".to_string());
    }
    formats
}

/// View an item.
async fn view_item(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Result<Html<String>, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Load item with view tap invocation
    let (item, render_outputs) = match state.items().load_for_view(id, &user).await {
        Ok(Some(data)) => data,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(JsonError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Render fields through filter pipeline
    let mut children_html = String::new();
    if let Some(fields) = item.fields.as_object() {
        for (name, value) in fields {
            // Compound field: has "sections" array
            if let Some(sections_raw) = value.get("sections").and_then(|s| s.as_array()) {
                // Sort sections by weight for correct display order
                let mut sorted_sections = sections_raw.clone();
                sorted_sections
                    .sort_by_key(|s| s.get("weight").and_then(|w| w.as_i64()).unwrap_or(0));

                for section in &sorted_sections {
                    let section_type = section
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown");

                    // Sanitize section_type for template suggestion: only allow
                    // alphanumeric, hyphens, and underscores to prevent path traversal
                    let safe_type: String = section_type
                        .chars()
                        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                        .collect();

                    // Process section data fields through FilterPipeline
                    let mut section_fields_html = String::new();
                    if let Some(data) = section.get("data").and_then(|d| d.as_object()) {
                        for (_key, val) in data {
                            if let (Some(text), Some(fmt)) = (
                                val.get("value").and_then(|v| v.as_str()),
                                val.get("format").and_then(|v| v.as_str()),
                            ) {
                                let filtered = FilterPipeline::for_format_safe(fmt).process(text);
                                section_fields_html.push_str(&filtered);
                            } else if let Some(text) = val.as_str() {
                                let filtered =
                                    FilterPipeline::for_format("plain_text").process(text);
                                section_fields_html.push_str(&filtered);
                            } else {
                                // Render non-string values (Integer, Float, Boolean) as
                                // escaped text so they're not silently dropped
                                if !val.is_object() && !val.is_array() && !val.is_null() {
                                    let text = val.to_string();
                                    let filtered =
                                        FilterPipeline::for_format("plain_text").process(&text);
                                    section_fields_html.push_str(&filtered);
                                }
                            }
                        }
                    }

                    // Try to resolve section template using sanitized type
                    let suggestions = [
                        format!("elements/compound-section--{safe_type}"),
                        "elements/compound-section".to_string(),
                    ];
                    let suggestion_refs: Vec<&str> =
                        suggestions.iter().map(|s| s.as_str()).collect();
                    let template = state
                        .theme()
                        .resolve_template(&suggestion_refs)
                        .unwrap_or_else(|| "elements/compound-section.html".to_string());

                    // Build sanitized section data: HTML-escape all string values
                    // so custom templates can safely use {{ section_data.field }}
                    let sanitized_data = if let Some(data) =
                        section.get("data").and_then(|d| d.as_object())
                    {
                        let mut clean = serde_json::Map::new();
                        for (k, v) in data {
                            if let Some(s) = v.as_str() {
                                clean.insert(k.clone(), serde_json::json!(html_escape(s)));
                            } else if let Some(obj) = v.as_object() {
                                // Escape string values inside nested objects like {value, format}
                                let mut inner = serde_json::Map::new();
                                for (ik, iv) in obj {
                                    if let Some(s) = iv.as_str() {
                                        inner.insert(ik.clone(), serde_json::json!(html_escape(s)));
                                    } else {
                                        inner.insert(ik.clone(), iv.clone());
                                    }
                                }
                                clean.insert(k.clone(), serde_json::Value::Object(inner));
                            } else {
                                clean.insert(k.clone(), v.clone());
                            }
                        }
                        serde_json::Value::Object(clean)
                    } else {
                        serde_json::json!({})
                    };

                    let mut section_ctx = tera::Context::new();
                    section_ctx.insert("section_data", &sanitized_data);
                    section_ctx.insert("section_type", &safe_type);
                    section_ctx.insert("section_body", &section_fields_html);

                    let section_html = state
                        .theme()
                        .tera()
                        .render(&template, &section_ctx)
                        .unwrap_or_else(|_| {
                            format!(
                                "<div class=\"compound-section compound-section--{}\">{}</div>",
                                html_escape(&safe_type),
                                section_fields_html
                            )
                        });
                    children_html.push_str(&section_html);
                }
            } else if let Some(text_val) = value.get("value").and_then(|v| v.as_str()) {
                let raw_fmt = value
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plain_text");
                let filtered = FilterPipeline::for_format_safe(raw_fmt).process(text_val);
                children_html.push_str(&format!(
                    "<div class=\"field field-{}\">{}</div>",
                    html_escape(name),
                    filtered
                ));
            }
        }
    }

    // Include plugin render outputs
    for output in render_outputs {
        children_html.push_str(&output);
    }

    // Resolve item template via theme engine
    let suggestions = [
        format!("elements/item--{}--{}", item.item_type, item.id),
        format!("elements/item--{}", item.item_type),
        "elements/item".to_string(),
    ];
    let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();
    let template = state
        .theme()
        .resolve_template(&suggestion_refs)
        .unwrap_or_else(|| "elements/item.html".to_string());

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("children", &children_html);

    let item_html = state
        .theme()
        .tera()
        .render(&template, &context)
        .unwrap_or_else(|_| {
            // Fallback if template rendering fails
            format!("<h1>{}</h1>{}", html_escape(&item.title), children_html)
        });

    // Wrap in page layout with site context
    let item_path = format!("/item/{id}");
    super::helpers::inject_site_context(&state, &session, &mut context, &item_path).await;

    let page_html = state
        .theme()
        .render_page(&item_path, &item.title, &item_html, &mut context)
        .unwrap_or_else(|_| format!("<!DOCTYPE html><html><body>{item_html}</body></html>"));

    Ok(Html(page_html))
}

/// Display add item form.
async fn add_item_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_type): Path<String>,
) -> Result<Html<String>, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Check permission
    let permission = format!("create {item_type} content");
    if !user.has_permission(&permission) && !user.is_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(JsonError {
                error: "Access denied".to_string(),
            }),
        ));
    }

    // Get content type definition
    let content_type = state.content_types().get(&item_type).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: format!("Content type '{item_type}' not found"),
            }),
        )
    })?;

    // Build form with format permissions
    let permitted_formats = permitted_text_formats(&user);
    let form_builder =
        FormBuilder::new(content_type.clone()).with_permitted_formats(permitted_formats);
    let form_html = form_builder.build_add_form(&format!("/item/add/{item_type}"));

    let html = format!(
        r#"<!DOCTYPE html><html><head>
        <title>Create {}</title>
        <style>
            body {{ font-family: sans-serif; max-width: 800px; margin: 0 auto; padding: 20px; }}
            .form-group {{ margin-bottom: 15px; }}
            label {{ display: block; margin-bottom: 5px; font-weight: bold; }}
            .form-control {{ width: 100%; padding: 8px; box-sizing: border-box; }}
            textarea.form-control {{ min-height: 200px; }}
            .btn {{ padding: 10px 20px; background: #007bff; color: white; border: none; cursor: pointer; }}
            .btn:hover {{ background: #0056b3; }}
            .form-help {{ font-size: 0.85em; color: #666; margin-top: 5px; }}
        </style>
        </head><body>
        <h1>Create {}</h1>
        {}
        </body></html>"#,
        html_escape(&content_type.label),
        html_escape(&content_type.label),
        form_html
    );

    Ok(Html(html))
}

/// Create a new item.
async fn create_item(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    resolved_lang: Option<Extension<crate::middleware::language::ResolvedLanguage>>,
    Path(item_type): Path<String>,
    Json(request): Json<CreateItemRequest>,
) -> Result<Json<ItemResponse>, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Check permission
    let permission = format!("create {item_type} content");
    if !user.has_permission(&permission) && !user.is_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(JsonError {
                error: "Access denied".to_string(),
            }),
        ));
    }

    // Verify CSRF token from header
    crate::routes::helpers::require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(JsonError {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    // Check content type exists
    if !state.content_types().exists(&item_type) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: format!("Content type '{item_type}' not found"),
            }),
        ));
    }

    let language = resolved_lang
        .map(|Extension(lang)| lang.0)
        .unwrap_or_else(|| state.default_language().to_string());

    let input = CreateItem {
        item_type: item_type.clone(),
        title: request.title,
        author_id: user.id,
        status: request.status,
        promote: None,
        sticky: None,
        fields: request.fields,
        stage_id: None,
        language: Some(language),
        log: request.log,
    };

    match state.items().create(input, &user).await {
        Ok(item) => {
            // Auto-generate URL alias if pattern configured for this type
            if let Err(e) = crate::services::pathauto::auto_alias_item(
                state.db(),
                item.id,
                &item.title,
                &item.item_type,
                item.created,
            )
            .await
            {
                tracing::warn!(error = %e, item_id = %item.id, "pathauto alias generation failed");
            }

            Ok(Json(ItemResponse {
                id: item.id,
                title: item.title,
                item_type: item.item_type,
                status: item.status,
            }))
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create item");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Failed to create item".to_string(),
                }),
            ))
        }
    }
}

/// Display edit item form.
async fn edit_item_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Result<Html<String>, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Load item
    let item = match state.items().load(id).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(JsonError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Check access
    if !state
        .items()
        .check_access(&item, "edit", &user)
        .await
        .unwrap_or(false)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(JsonError {
                error: "Access denied".to_string(),
            }),
        ));
    }

    // Get content type definition
    let content_type = state.content_types().get(&item.item_type).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: format!("Content type '{}' not found", item.item_type),
            }),
        )
    })?;

    // Build form with format permissions
    let permitted_formats = permitted_text_formats(&user);
    let form_builder =
        FormBuilder::new(content_type.clone()).with_permitted_formats(permitted_formats);
    let form_html = form_builder.build_edit_form(&item, &format!("/item/{id}/edit"));

    // Get current URL alias for this item
    let source = format!("/item/{id}");
    let current_alias = UrlAlias::get_canonical_alias(state.db(), &source)
        .await
        .unwrap_or(None)
        .unwrap_or_default();

    // Build URL alias field HTML
    let alias_field = format!(
        r#"<div class="form-group">
            <label for="url_alias">URL Alias</label>
            <input type="text" id="url_alias" name="url_alias" class="form-control"
                   value="{}" placeholder="/about-us">
            <p class="form-help">Human-readable URL path. Leave empty for default (/item/id)</p>
        </div>"#,
        html_escape(&current_alias)
    );

    let html = format!(
        r#"<!DOCTYPE html><html><head>
        <title>Edit {}</title>
        <style>
            body {{ font-family: sans-serif; max-width: 800px; margin: 0 auto; padding: 20px; }}
            .form-group {{ margin-bottom: 15px; }}
            label {{ display: block; margin-bottom: 5px; font-weight: bold; }}
            .form-control {{ width: 100%; padding: 8px; box-sizing: border-box; }}
            textarea.form-control {{ min-height: 200px; }}
            .btn {{ padding: 10px 20px; background: #007bff; color: white; border: none; cursor: pointer; }}
            .btn:hover {{ background: #0056b3; }}
            .form-help {{ font-size: 0.85em; color: #666; margin-top: 5px; }}
        </style>
        </head><body>
        <h1>Edit: {}</h1>
        {}
        {}
        </body></html>"#,
        html_escape(&item.title),
        html_escape(&item.title),
        form_html,
        alias_field
    );

    Ok(Html(html))
}

/// Update an existing item.
async fn update_item(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateItemRequest>,
) -> Result<Json<ItemResponse>, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Verify CSRF token from header
    crate::routes::helpers::require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(JsonError {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    let input = UpdateItem {
        title: request.title,
        status: request.status,
        promote: None,
        sticky: None,
        fields: request.fields,
        log: request.log,
    };

    match state.items().update(id, input, &user).await {
        Ok(Some(item)) => {
            // Handle URL alias update if provided
            if let Some(alias_path) = request.url_alias {
                let source = format!("/item/{id}");
                let alias_path = alias_path.trim();

                if alias_path.is_empty() {
                    // Delete existing alias if path is cleared
                    if let Err(e) = UrlAlias::delete_by_source(state.db(), &source).await {
                        tracing::warn!(error = %e, "failed to delete url alias");
                    }
                } else {
                    // Ensure alias starts with /
                    let alias_path = if alias_path.starts_with('/') {
                        alias_path.to_string()
                    } else {
                        format!("/{alias_path}")
                    };

                    // Create or update alias
                    if let Err(e) =
                        UrlAlias::upsert_for_source(state.db(), &source, &alias_path, "live", "en")
                            .await
                    {
                        tracing::warn!(error = %e, "failed to update url alias");
                    }
                }
            } else {
                // No explicit alias — auto-update from pathauto pattern
                if let Err(e) = crate::services::pathauto::update_alias_item(
                    state.db(),
                    item.id,
                    &item.title,
                    &item.item_type,
                    item.created,
                )
                .await
                {
                    tracing::warn!(error = %e, item_id = %item.id, "pathauto alias update failed");
                }
            }

            Ok(Json(ItemResponse {
                id: item.id,
                title: item.title,
                item_type: item.item_type,
                status: item.status,
            }))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Item not found".to_string(),
            }),
        )),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("access denied") {
                Err((
                    StatusCode::FORBIDDEN,
                    Json(JsonError {
                        error: "Access denied".to_string(),
                    }),
                ))
            } else {
                tracing::error!(error = %e, "failed to update item");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(JsonError {
                        error: "Failed to update item".to_string(),
                    }),
                ))
            }
        }
    }
}

/// Delete an item.
async fn delete_item(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Verify CSRF token from header
    crate::routes::helpers::require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(JsonError {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    match state.items().delete(id, &user).await {
        Ok(true) => Ok(Json(serde_json::json!({"deleted": true}))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Item not found".to_string(),
            }),
        )),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("access denied") {
                Err((
                    StatusCode::FORBIDDEN,
                    Json(JsonError {
                        error: "Access denied".to_string(),
                    }),
                ))
            } else {
                tracing::error!(error = %e, "failed to delete item");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(JsonError {
                        error: "Failed to delete item".to_string(),
                    }),
                ))
            }
        }
    }
}

/// List revision history for an item.
async fn list_revisions(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Result<Html<String>, (StatusCode, Json<JsonError>)> {
    let _user = get_user_context(&session, &state).await;

    // Load item
    let item = match state.items().load(id).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(JsonError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Get revisions
    let revisions = state.items().get_revisions(id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to get revisions");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    // Build HTML
    let mut html = String::new();
    html.push_str("<!DOCTYPE html><html><head>");
    html.push_str(&format!(
        "<title>Revisions: {}</title>",
        html_escape(&item.title)
    ));
    html.push_str("<style>body { font-family: sans-serif; max-width: 800px; margin: 0 auto; padding: 20px; } table { width: 100%; border-collapse: collapse; } th, td { padding: 10px; text-align: left; border-bottom: 1px solid #ddd; } .btn { padding: 5px 10px; background: #007bff; color: white; text-decoration: none; border-radius: 3px; }</style>");
    html.push_str("</head><body>");

    html.push_str(&format!("<h1>Revisions: {}</h1>", html_escape(&item.title)));
    html.push_str(&format!(
        r#"<p><a href="/item/{id}">← Back to item</a></p>"#
    ));

    // Generate CSRF token for revert forms
    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    html.push_str("<table><thead><tr><th>Date</th><th>Title</th><th>Log</th><th>Actions</th></tr></thead><tbody>");

    for rev in revisions {
        let date = chrono::DateTime::from_timestamp(rev.created, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        let current = if Some(rev.id) == item.current_revision_id {
            " (current)"
        } else {
            ""
        };
        let log = rev.log.as_deref().unwrap_or("-");
        let revert_btn = if Some(rev.id) != item.current_revision_id {
            format!(
                r#"<form method="post" action="/item/{}/revert/{}" style="display:inline"><input type="hidden" name="_token" value="{}"><button type="submit" class="btn">Revert</button></form>"#,
                id,
                rev.id,
                html_escape(&csrf_token)
            )
        } else {
            String::new()
        };

        html.push_str(&format!(
            "<tr><td>{}{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            date,
            current,
            html_escape(&rev.title),
            html_escape(log),
            revert_btn
        ));
    }

    html.push_str("</tbody></table></body></html>");

    Ok(Html(html))
}

/// Revert to a previous revision.
async fn revert_revision(
    State(state): State<AppState>,
    session: Session,
    Path((id, rev_id)): Path<(Uuid, Uuid)>,
    Form(form): Form<CsrfOnlyForm>,
) -> Result<impl IntoResponse, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Verify CSRF token from form body
    crate::routes::helpers::require_csrf(&session, &form.token)
        .await
        .map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                Json(JsonError {
                    error: "Invalid or expired form token. Please try again.".to_string(),
                }),
            )
        })?;

    match state.items().revert_to_revision(id, rev_id, &user).await {
        Ok(_) => Ok(Redirect::to(&format!("/item/{id}/revisions"))),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("access denied") {
                Err((
                    StatusCode::FORBIDDEN,
                    Json(JsonError {
                        error: "Access denied".to_string(),
                    }),
                ))
            } else {
                tracing::error!(error = %e, "failed to revert revision");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(JsonError {
                        error: "Failed to revert".to_string(),
                    }),
                ))
            }
        }
    }
}

/// List all content types (API endpoint).
async fn list_content_types(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.content_types().type_names())
}

/// List items by type (API endpoint).
async fn list_items_by_type(
    State(state): State<AppState>,
    Path(item_type): Path<String>,
) -> Result<Json<Vec<ItemResponse>>, (StatusCode, Json<JsonError>)> {
    match state.items().list_by_type(&item_type).await {
        Ok(items) => Ok(Json(
            items
                .into_iter()
                .map(|i| ItemResponse {
                    id: i.id,
                    title: i.title,
                    item_type: i.item_type,
                    status: i.status,
                })
                .collect(),
        )),
        Err(e) => {
            tracing::error!(error = %e, "failed to list items");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Failed to list items".to_string(),
                }),
            ))
        }
    }
}

// =============================================================================
// JSON API Endpoints
// =============================================================================

/// Get a single item by ID (JSON API).
///
/// GET /api/item/{id}?include=author
async fn get_item_api(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<GetItemQuery>,
) -> Result<Json<ItemApiResponse>, (StatusCode, Json<JsonError>)> {
    // Load item
    let item = match state.items().load(id).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(JsonError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Check if we should include author
    let include_author = query
        .include
        .as_ref()
        .map(|s| s.split(',').any(|part| part.trim() == "author"))
        .unwrap_or(false);

    let author = if include_author {
        match User::find_by_id(state.db(), item.author_id).await {
            Ok(Some(user)) => Some(AuthorResponse {
                id: user.id,
                name: user.name,
            }),
            _ => None,
        }
    } else {
        None
    };

    Ok(Json(ItemApiResponse {
        id: item.id,
        item_type: item.item_type,
        title: item.title,
        status: item.status,
        author_id: item.author_id,
        author,
        created: item.created,
        changed: item.changed,
        promote: item.promote,
        sticky: item.sticky,
        fields: item.fields,
        stage_id: item.stage_id,
    }))
}

/// List items with filtering and pagination (JSON API).
///
/// GET /api/items?type=article&status=1&page=1&per_page=20&include=author
async fn list_items_api(
    State(state): State<AppState>,
    Query(query): Query<ListItemsQuery>,
) -> Result<Json<PaginatedResponse<ItemApiResponse>>, (StatusCode, Json<JsonError>)> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * per_page;

    // Check if we should include author
    let include_author = query
        .include
        .as_ref()
        .map(|s| s.split(',').any(|part| part.trim() == "author"))
        .unwrap_or(false);

    // Build query with filters
    let (items, total) = state
        .items()
        .list_filtered(
            query.item_type.as_deref(),
            query.status,
            query.author_id,
            per_page,
            offset,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list items");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Failed to list items".to_string(),
                }),
            )
        })?;

    // Optionally load authors
    let mut author_cache: std::collections::HashMap<Uuid, AuthorResponse> =
        std::collections::HashMap::new();
    if include_author {
        let author_ids: Vec<Uuid> = items.iter().map(|i| i.author_id).collect();
        for author_id in author_ids {
            if !author_cache.contains_key(&author_id)
                && let Ok(Some(user)) = User::find_by_id(state.db(), author_id).await
            {
                author_cache.insert(
                    author_id,
                    AuthorResponse {
                        id: user.id,
                        name: user.name,
                    },
                );
            }
        }
    }

    let total_pages = (total as f64 / per_page as f64).ceil() as i64;

    let items_response: Vec<ItemApiResponse> = items
        .into_iter()
        .map(|item| {
            let author = if include_author {
                author_cache.get(&item.author_id).cloned()
            } else {
                None
            };
            ItemApiResponse {
                id: item.id,
                item_type: item.item_type,
                title: item.title,
                status: item.status,
                author_id: item.author_id,
                author,
                created: item.created,
                changed: item.changed,
                promote: item.promote,
                sticky: item.sticky,
                fields: item.fields,
                stage_id: item.stage_id,
            }
        })
        .collect();

    Ok(Json(PaginatedResponse {
        items: items_response,
        pagination: PaginationMeta {
            total,
            page,
            per_page,
            total_pages,
        },
    }))
}
