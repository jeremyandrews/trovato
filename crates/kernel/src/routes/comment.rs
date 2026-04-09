//! Comment routes for threaded discussions.
//!
//! Provides endpoints for viewing, creating, and moderating comments on content items.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post, put},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::FilterPipeline;
use crate::models::{Comment, CreateComment, UpdateComment};
use crate::routes::auth::SESSION_USER_ID;
use crate::routes::helpers::{JsonError, require_csrf_header};
use crate::state::AppState;
use crate::tap::UserContext;

/// Render a comment body to HTML with safe format whitelisting.
fn render_comment_body(comment: &Comment) -> String {
    FilterPipeline::for_format_safe(&comment.body_format).process(&comment.body)
}

// =============================================================================
// Response Types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct CommentResponse {
    pub id: Uuid,
    pub item_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<AuthorInfo>,
    pub body: String,
    pub body_html: String,
    pub status: i16,
    pub created: i64,
    pub changed: i64,
    pub depth: i16,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorInfo {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CommentListResponse {
    pub comments: Vec<CommentResponse>,
    pub total: i64,
}

// =============================================================================
// Request Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateCommentRequest {
    pub body: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCommentRequest {
    pub body: Option<String>,
    pub status: Option<i16>,
}

#[derive(Debug, Deserialize)]
pub struct ListCommentsQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub include: Option<String>,
}

// =============================================================================
// Public API Routes
// =============================================================================

/// List comments for an item.
///
/// GET /api/item/{id}/comments
async fn list_item_comments(
    State(state): State<AppState>,
    Path(item_id): Path<Uuid>,
    Query(query): Query<ListCommentsQuery>,
) -> Result<Json<CommentListResponse>, (StatusCode, Json<JsonError>)> {
    // Verify item exists
    let item = state.items().load(item_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load item");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    if item.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Item not found".to_string(),
            }),
        ));
    }

    let include_author = query
        .include
        .as_ref()
        .map(|s| s.split(',').any(|part| part.trim() == "author"))
        .unwrap_or(false);

    // Get comments (threaded order)
    let comments = state.comments().list_for_item(item_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to list comments");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let total = comments.len() as i64;

    // Build response with optional author info
    let mut comment_responses = Vec::with_capacity(comments.len());
    let mut author_cache: std::collections::HashMap<Uuid, AuthorInfo> =
        std::collections::HashMap::new();

    for comment in comments {
        let author = if include_author {
            if let Some(cached) = author_cache.get(&comment.author_id) {
                Some(cached.clone())
            } else if let Ok(Some(user)) = state.users().find_by_id(comment.author_id).await {
                let info = AuthorInfo {
                    id: user.id,
                    name: user.name.clone(),
                };
                author_cache.insert(comment.author_id, info.clone());
                Some(info)
            } else {
                None
            }
        } else {
            None
        };

        let body_html = render_comment_body(&comment);

        comment_responses.push(CommentResponse {
            id: comment.id,
            item_id: comment.item_id,
            parent_id: comment.parent_id,
            author_id: comment.author_id,
            author,
            body: comment.body,
            body_html,
            status: comment.status,
            created: comment.created,
            changed: comment.changed,
            depth: comment.depth,
        });
    }

    Ok(Json(CommentListResponse {
        comments: comment_responses,
        total,
    }))
}

/// Create a comment on an item.
///
/// POST /api/item/{id}/comments
async fn create_comment(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Json(request): Json<CreateCommentRequest>,
) -> Result<Json<CommentResponse>, (StatusCode, Json<JsonError>)> {
    // Check authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let user_id = user_id.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(JsonError {
                error: "Authentication required".to_string(),
            }),
        )
    })?;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(JsonError {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    // Verify item exists (used for notification below)
    let item = state
        .items()
        .load(item_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to load item");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(JsonError {
                    error: "Item not found".to_string(),
                }),
            )
        })?;

    // Verify parent comment exists if specified
    if let Some(parent_id) = request.parent_id {
        let parent = state.comments().load(parent_id).await.map_err(|e| {
            tracing::error!(error = %e, "failed to load parent comment");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;

        let Some(parent) = parent else {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(JsonError {
                    error: "Parent comment not found".to_string(),
                }),
            ));
        };

        // Verify parent is on the same item
        if parent.item_id != item_id {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(JsonError {
                    error: "Parent comment is on a different item".to_string(),
                }),
            ));
        }
    }

    // Validate body
    if request.body.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(JsonError {
                error: "Comment body cannot be empty".to_string(),
            }),
        ));
    }

    // Build UserContext with real permissions for access check
    let user = state.users().find_by_id(user_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load user");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;
    let user = user.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(JsonError {
                error: "User not found".to_string(),
            }),
        )
    })?;
    let user_perms = state
        .permissions()
        .load_user_permissions(&user)
        .await
        .unwrap_or_default();
    let user_ctx = if user.is_admin {
        UserContext::authenticated(user_id, {
            let mut p: Vec<String> = user_perms.into_iter().collect();
            p.push("administer site".to_string());
            p
        })
    } else {
        UserContext::authenticated(user_id, user_perms.into_iter().collect())
    };

    // Check "post comments" permission
    if !user_ctx.is_admin() && !user_ctx.has_permission("post comments") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(JsonError {
                error: "You do not have permission to post comments".to_string(),
            }),
        ));
    }

    // Create comment
    let input = CreateComment {
        item_id,
        parent_id: request.parent_id,
        author_id: user_id,
        body: request.body.clone(),
        body_format: Some("filtered_html".to_string()),
        status: Some(1), // Published by default
    };
    let comment = state
        .comments()
        .create(input, &user_ctx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create comment");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Failed to create comment".to_string(),
                }),
            )
        })?;

    // Get commenter info
    let commenter = state
        .users()
        .find_by_id(user_id)
        .await
        .ok()
        .flatten()
        .map(|u| AuthorInfo {
            id: u.id,
            name: u.name,
        });

    // Send comment notification to content author (non-blocking)
    if let Some(email_service) = state.email() {
        // Only notify when commenter is not the content author
        if comment.author_id != item.author_id {
            let notification_state = state.clone();
            let email = email_service.clone();
            let comment_body = comment.body.clone();
            let item_title = item.title.clone();
            let item_author_id = item.author_id;
            let commenter_name = commenter
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "Someone".to_string());

            tokio::spawn(async move {
                send_comment_notification(
                    &notification_state,
                    &email,
                    item_author_id,
                    &commenter_name,
                    &item_title,
                    &comment_body,
                    item_id,
                )
                .await;
            });
        }
    }

    let body_html = render_comment_body(&comment);

    Ok(Json(CommentResponse {
        id: comment.id,
        item_id: comment.item_id,
        parent_id: comment.parent_id,
        author_id: comment.author_id,
        author: commenter,
        body: comment.body,
        body_html,
        status: comment.status,
        created: comment.created,
        changed: comment.changed,
        depth: comment.depth,
    }))
}

/// Get a single comment.
///
/// GET /api/comment/{id}
async fn get_comment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ListCommentsQuery>,
) -> Result<Json<CommentResponse>, (StatusCode, Json<JsonError>)> {
    let comment = state.comments().load(id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let comment = comment.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Comment not found".to_string(),
            }),
        )
    })?;

    let include_author = query
        .include
        .as_ref()
        .map(|s| s.split(',').any(|part| part.trim() == "author"))
        .unwrap_or(false);

    let author = if include_author {
        state
            .users()
            .find_by_id(comment.author_id)
            .await
            .ok()
            .flatten()
            .map(|u| AuthorInfo {
                id: u.id,
                name: u.name,
            })
    } else {
        None
    };

    let body_html = render_comment_body(&comment);

    Ok(Json(CommentResponse {
        id: comment.id,
        item_id: comment.item_id,
        parent_id: comment.parent_id,
        author_id: comment.author_id,
        author,
        body: comment.body,
        body_html,
        status: comment.status,
        created: comment.created,
        changed: comment.changed,
        depth: comment.depth,
    }))
}

/// Update a comment.
///
/// PUT /api/comment/{id}
async fn update_comment(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateCommentRequest>,
) -> Result<Json<CommentResponse>, (StatusCode, Json<JsonError>)> {
    // Check authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let user_id = user_id.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(JsonError {
                error: "Authentication required".to_string(),
            }),
        )
    })?;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(JsonError {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    // Load existing comment
    let existing = state.comments().load(id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let existing = existing.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Comment not found".to_string(),
            }),
        )
    })?;

    // Build UserContext for the acting user to check access
    let user = state.users().find_by_id(user_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load user");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;
    let user = user.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(JsonError {
                error: "User not found".to_string(),
            }),
        )
    })?;
    let user_perms = state
        .permissions()
        .load_user_permissions(&user)
        .await
        .unwrap_or_default();
    let user_ctx = if user.is_admin {
        UserContext::authenticated(user_id, {
            let mut p: Vec<String> = user_perms.into_iter().collect();
            p.push("administer site".to_string());
            p
        })
    } else {
        UserContext::authenticated(user_id, user_perms.into_iter().collect())
    };

    // Check permission via service (admin, tap, or permission fallback)
    let has_access = state
        .comments()
        .check_access(&existing, "edit", &user_ctx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to check comment access");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;
    if !has_access {
        return Err((
            StatusCode::FORBIDDEN,
            Json(JsonError {
                error: "You do not have permission to edit this comment".to_string(),
            }),
        ));
    }

    // Validate body if provided
    if let Some(ref body) = request.body
        && body.trim().is_empty()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(JsonError {
                error: "Comment body cannot be empty".to_string(),
            }),
        ));
    }

    let input = UpdateComment {
        body: request.body,
        body_format: None,
        status: request.status,
    };

    let comment = state
        .comments()
        .update(id, input, &user_ctx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to update comment");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Failed to update comment".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(JsonError {
                    error: "Comment not found".to_string(),
                }),
            )
        })?;

    let author = state
        .users()
        .find_by_id(comment.author_id)
        .await
        .ok()
        .flatten()
        .map(|u| AuthorInfo {
            id: u.id,
            name: u.name,
        });

    let body_html = render_comment_body(&comment);

    Ok(Json(CommentResponse {
        id: comment.id,
        item_id: comment.item_id,
        parent_id: comment.parent_id,
        author_id: comment.author_id,
        author,
        body: comment.body,
        body_html,
        status: comment.status,
        created: comment.created,
        changed: comment.changed,
        depth: comment.depth,
    }))
}

/// Delete a comment.
///
/// DELETE /api/comment/{id}
async fn delete_comment(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<JsonError>)> {
    // Check authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let user_id = user_id.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(JsonError {
                error: "Authentication required".to_string(),
            }),
        )
    })?;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|(s, j)| {
            (
                s,
                Json(JsonError {
                    error: j.0["error"].as_str().unwrap_or("CSRF error").to_string(),
                }),
            )
        })?;

    // Load existing comment
    let existing = state.comments().load(id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let existing = existing.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Comment not found".to_string(),
            }),
        )
    })?;

    // Build UserContext for access check
    let user = state.users().find_by_id(user_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load user");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;
    let user = user.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(JsonError {
                error: "User not found".to_string(),
            }),
        )
    })?;
    let user_perms = state
        .permissions()
        .load_user_permissions(&user)
        .await
        .unwrap_or_default();
    let user_ctx = if user.is_admin {
        UserContext::authenticated(user_id, {
            let mut p: Vec<String> = user_perms.into_iter().collect();
            p.push("administer site".to_string());
            p
        })
    } else {
        UserContext::authenticated(user_id, user_perms.into_iter().collect())
    };

    // Check permission via service
    let has_access = state
        .comments()
        .check_access(&existing, "delete", &user_ctx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to check comment access");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            )
        })?;
    if !has_access {
        return Err((
            StatusCode::FORBIDDEN,
            Json(JsonError {
                error: "You do not have permission to delete this comment".to_string(),
            }),
        ));
    }

    state.comments().delete(id, &user_ctx).await.map_err(|e| {
        tracing::error!(error = %e, "failed to delete comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Failed to delete comment".to_string(),
            }),
        )
    })?;

    Ok(Json(serde_json::json!({"deleted": true})))
}

// =============================================================================
// Notification helpers
// =============================================================================

/// Send a comment notification email to the content author.
///
/// This is called in a background task and must not panic. All errors
/// are logged but silently swallowed.
async fn send_comment_notification(
    state: &AppState,
    email_service: &std::sync::Arc<crate::services::email::EmailService>,
    item_author_id: uuid::Uuid,
    commenter_name: &str,
    item_title: &str,
    comment_text: &str,
    item_id: uuid::Uuid,
) {
    // Load the content author's email
    let author = match state.users().find_by_id(item_author_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            tracing::debug!("comment notification: author not found");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "comment notification: failed to load author");
            return;
        }
    };

    if author.mail.is_empty() {
        return;
    }

    let site_name = crate::models::SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let site_url = email_service.site_url();
    let action_url = format!("{site_url}/item/{item_id}");
    let subject = format!("New comment on \"{item_title}\" at {site_name}");

    // Truncate comment preview for email
    let preview: &str = if comment_text.len() > 500 {
        &comment_text[..500]
    } else {
        comment_text
    };

    let mut context = tera::Context::new();
    context.insert("site_name", &site_name);
    context.insert("commenter_name", commenter_name);
    context.insert("content_title", item_title);
    context.insert("comment_text", preview);
    context.insert("action_url", &action_url);
    context.insert("subject", &subject);

    let tera = state.theme().tera();
    match crate::services::email_templates::render(tera, "comment_notification", &context) {
        Ok((html, text)) => {
            if let Err(e) = email_service
                .send_templated(&author.mail, &subject, &text, html.as_deref())
                .await
            {
                tracing::warn!(error = %e, "comment notification: failed to send email");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "comment notification: failed to render template");
        }
    }
}

// =============================================================================
// Router
// =============================================================================

/// Create the comment router.
pub fn router() -> Router<AppState> {
    Router::new()
        // Public API
        .route("/api/item/{id}/comments", get(list_item_comments))
        .route("/api/item/{id}/comments", post(create_comment))
        .route("/api/comment/{id}", get(get_comment))
        .route("/api/comment/{id}", put(update_comment))
        .route("/api/comment/{id}", delete(delete_comment))
}
