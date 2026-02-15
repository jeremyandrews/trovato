//! Item CRUD route handlers.
//!
//! Provides endpoints for viewing, creating, editing, and deleting content items.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::{FilterPipeline, FormBuilder};
use crate::models::{CreateItem, UpdateItem, UrlAlias, User};
use crate::state::AppState;
use crate::tap::UserContext;

use super::helpers::html_escape;

/// Session key for user ID.
const SESSION_USER_ID: &str = "user_id";

/// Error response for item operations.
#[derive(Debug, Serialize)]
pub struct ItemError {
    pub error: String,
}

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

/// View an item.
async fn view_item(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Result<Html<String>, (StatusCode, Json<ItemError>)> {
    let user = get_user_context(&session, &state).await;

    // Load item with view tap invocation
    let (item, render_outputs) = match state.items().load_for_view(id, &user).await {
        Ok(Some(data)) => data,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ItemError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ItemError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Render fields through filter pipeline
    let mut children_html = String::new();
    if let Some(fields) = item.fields.as_object() {
        for (name, value) in fields {
            if let Some(text_val) = value.get("value").and_then(|v| v.as_str()) {
                let format = value
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plain_text");
                let filtered = FilterPipeline::for_format(format).process(text_val);
                children_html.push_str(&format!(
                    "<div class=\"field field-{}\">{}</div>",
                    name, filtered
                ));
            }
        }
    }

    // Include plugin render outputs
    for output in render_outputs {
        children_html.push_str(&output);
    }

    // Resolve item template via theme engine
    let suggestions = vec![
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
            format!(
                "<h1>{}</h1>{}",
                html_escape(&item.title),
                children_html
            )
        });

    // Wrap in page layout with site context
    let item_path = format!("/item/{}", id);
    super::helpers::inject_site_context(&state, &session, &mut context).await;

    let page_html = state
        .theme()
        .render_page(&item_path, &item.title, &item_html, &mut context)
        .unwrap_or_else(|_| {
            format!("<!DOCTYPE html><html><body>{}</body></html>", item_html)
        });

    Ok(Html(page_html))
}

/// Display add item form.
async fn add_item_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_type): Path<String>,
) -> Result<Html<String>, (StatusCode, Json<ItemError>)> {
    let user = get_user_context(&session, &state).await;

    // Check permission
    let permission = format!("create {} content", item_type);
    if !user.has_permission(&permission) && !user.is_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ItemError {
                error: "Access denied".to_string(),
            }),
        ));
    }

    // Get content type definition
    let content_type = state.content_types().get(&item_type).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ItemError {
                error: format!("Content type '{}' not found", item_type),
            }),
        )
    })?;

    // Build form
    let form_builder = FormBuilder::new(content_type.clone());
    let form_html = form_builder.build_add_form(&format!("/item/add/{}", item_type));

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
    Path(item_type): Path<String>,
    Json(request): Json<CreateItemRequest>,
) -> Result<Json<ItemResponse>, (StatusCode, Json<ItemError>)> {
    let user = get_user_context(&session, &state).await;

    // Check permission
    let permission = format!("create {} content", item_type);
    if !user.has_permission(&permission) && !user.is_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ItemError {
                error: "Access denied".to_string(),
            }),
        ));
    }

    // Check content type exists
    if !state.content_types().exists(&item_type) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ItemError {
                error: format!("Content type '{}' not found", item_type),
            }),
        ));
    }

    let input = CreateItem {
        item_type: item_type.clone(),
        title: request.title,
        author_id: user.id,
        status: request.status,
        promote: None,
        sticky: None,
        fields: request.fields,
        stage_id: None,
        log: request.log,
    };

    match state.items().create(input, &user).await {
        Ok(item) => Ok(Json(ItemResponse {
            id: item.id,
            title: item.title,
            item_type: item.item_type,
            status: item.status,
        })),
        Err(e) => {
            tracing::error!(error = %e, "failed to create item");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ItemError {
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
) -> Result<Html<String>, (StatusCode, Json<ItemError>)> {
    let user = get_user_context(&session, &state).await;

    // Load item
    let item = match state.items().load(id).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ItemError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ItemError {
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
            Json(ItemError {
                error: "Access denied".to_string(),
            }),
        ));
    }

    // Get content type definition
    let content_type = state.content_types().get(&item.item_type).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ItemError {
                error: format!("Content type '{}' not found", item.item_type),
            }),
        )
    })?;

    // Build form
    let form_builder = FormBuilder::new(content_type.clone());
    let form_html = form_builder.build_edit_form(&item, &format!("/item/{}/edit", id));

    // Get current URL alias for this item
    let source = format!("/item/{}", id);
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
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateItemRequest>,
) -> Result<Json<ItemResponse>, (StatusCode, Json<ItemError>)> {
    let user = get_user_context(&session, &state).await;

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
                let source = format!("/item/{}", id);
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
                        format!("/{}", alias_path)
                    };

                    // Create or update alias
                    if let Err(e) =
                        UrlAlias::upsert_for_source(state.db(), &source, &alias_path, "live", "en")
                            .await
                    {
                        tracing::warn!(error = %e, "failed to update url alias");
                    }
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
            Json(ItemError {
                error: "Item not found".to_string(),
            }),
        )),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("access denied") {
                Err((
                    StatusCode::FORBIDDEN,
                    Json(ItemError {
                        error: "Access denied".to_string(),
                    }),
                ))
            } else {
                tracing::error!(error = %e, "failed to update item");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ItemError {
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
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ItemError>)> {
    let user = get_user_context(&session, &state).await;

    match state.items().delete(id, &user).await {
        Ok(true) => Ok(Json(serde_json::json!({"deleted": true}))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(ItemError {
                error: "Item not found".to_string(),
            }),
        )),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("access denied") {
                Err((
                    StatusCode::FORBIDDEN,
                    Json(ItemError {
                        error: "Access denied".to_string(),
                    }),
                ))
            } else {
                tracing::error!(error = %e, "failed to delete item");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ItemError {
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
) -> Result<Html<String>, (StatusCode, Json<ItemError>)> {
    let _user = get_user_context(&session, &state).await;

    // Load item
    let item = match state.items().load(id).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ItemError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ItemError {
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
            Json(ItemError {
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
        r#"<p><a href="/item/{}">← Back to item</a></p>"#,
        id
    ));

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
                r#"<form method="post" action="/item/{}/revert/{}" style="display:inline"><button type="submit" class="btn">Revert</button></form>"#,
                id, rev.id
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
) -> Result<impl IntoResponse, (StatusCode, Json<ItemError>)> {
    let user = get_user_context(&session, &state).await;

    match state.items().revert_to_revision(id, rev_id, &user).await {
        Ok(_) => Ok(Redirect::to(&format!("/item/{}/revisions", id))),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("access denied") {
                Err((
                    StatusCode::FORBIDDEN,
                    Json(ItemError {
                        error: "Access denied".to_string(),
                    }),
                ))
            } else {
                tracing::error!(error = %e, "failed to revert revision");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ItemError {
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
) -> Result<Json<Vec<ItemResponse>>, (StatusCode, Json<ItemError>)> {
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
                Json(ItemError {
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
) -> Result<Json<ItemApiResponse>, (StatusCode, Json<ItemError>)> {
    // Load item
    let item = match state.items().load(id).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ItemError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ItemError {
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
) -> Result<Json<PaginatedResponse<ItemApiResponse>>, (StatusCode, Json<ItemError>)> {
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
                Json(ItemError {
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
            if !author_cache.contains_key(&author_id) {
                if let Ok(Some(user)) = User::find_by_id(state.db(), author_id).await {
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
