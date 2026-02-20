//! Comment routes for threaded discussions.
//!
//! Provides endpoints for viewing, creating, and moderating comments on content items.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post, put},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::FilterPipeline;
use crate::models::{Comment, CreateComment, Item, UpdateComment, User};
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

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

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
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
) -> Result<Json<CommentListResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Verify item exists
    let item = Item::find_by_id(state.db(), item_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load item");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    if item.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
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
    let comments = Comment::list_for_item(state.db(), item_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list comments");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
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
            } else if let Ok(Some(user)) = User::find_by_id(state.db(), comment.author_id).await {
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
    Path(item_id): Path<Uuid>,
    Json(request): Json<CreateCommentRequest>,
) -> Result<Json<CommentResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Check authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let user_id = user_id.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Authentication required".to_string(),
            }),
        )
    })?;

    // Verify item exists
    let item = Item::find_by_id(state.db(), item_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load item");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    if item.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Item not found".to_string(),
            }),
        ));
    }

    // Verify parent comment exists if specified
    if let Some(parent_id) = request.parent_id {
        let parent = Comment::find_by_id(state.db(), parent_id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to load parent comment");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Internal server error".to_string(),
                    }),
                )
            })?;

        let Some(parent) = parent else {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Parent comment not found".to_string(),
                }),
            ));
        };

        // Verify parent is on the same item
        if parent.item_id != item_id {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Parent comment is on a different item".to_string(),
                }),
            ));
        }
    }

    // Validate body
    if request.body.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Comment body cannot be empty".to_string(),
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

    let comment = Comment::create(state.db(), input).await.map_err(|e| {
        tracing::error!(error = %e, "failed to create comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to create comment".to_string(),
            }),
        )
    })?;

    // Get author info
    let author = User::find_by_id(state.db(), user_id)
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

/// Get a single comment.
///
/// GET /api/comment/{id}
async fn get_comment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ListCommentsQuery>,
) -> Result<Json<CommentResponse>, (StatusCode, Json<ErrorResponse>)> {
    let comment = Comment::find_by_id(state.db(), id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let comment = comment.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
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
        User::find_by_id(state.db(), comment.author_id)
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
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateCommentRequest>,
) -> Result<Json<CommentResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Check authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let user_id = user_id.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Authentication required".to_string(),
            }),
        )
    })?;

    // Load existing comment
    let existing = Comment::find_by_id(state.db(), id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let existing = existing.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Comment not found".to_string(),
            }),
        )
    })?;

    // Check permission (must be author or admin)
    // TODO: Add proper permission check for admins
    if existing.author_id != user_id {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "You can only edit your own comments".to_string(),
            }),
        ));
    }

    // Validate body if provided
    if let Some(ref body) = request.body
        && body.trim().is_empty()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Comment body cannot be empty".to_string(),
            }),
        ));
    }

    let input = UpdateComment {
        body: request.body,
        body_format: None,
        status: request.status,
    };

    let comment = Comment::update(state.db(), id, input)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to update comment");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to update comment".to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Comment not found".to_string(),
                }),
            )
        })?;

    let author = User::find_by_id(state.db(), comment.author_id)
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
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    // Check authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let user_id = user_id.ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Authentication required".to_string(),
            }),
        )
    })?;

    // Load existing comment
    let existing = Comment::find_by_id(state.db(), id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let existing = existing.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Comment not found".to_string(),
            }),
        )
    })?;

    // Check permission (must be author or admin)
    // TODO: Add proper permission check for admins
    if existing.author_id != user_id {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "You can only delete your own comments".to_string(),
            }),
        ));
    }

    Comment::delete(state.db(), id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to delete comment");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to delete comment".to_string(),
            }),
        )
    })?;

    Ok(Json(serde_json::json!({"deleted": true})))
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
