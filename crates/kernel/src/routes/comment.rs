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
use crate::models::{Comment, CommentStatus, CreateComment, UpdateComment};
use crate::routes::auth::SESSION_USER_ID;
use crate::routes::helpers::{JsonError, require_csrf_header, user_context_for};
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
    session: Session,
    Path(item_id): Path<Uuid>,
    Query(query): Query<ListCommentsQuery>,
) -> Result<Json<CommentListResponse>, (StatusCode, Json<JsonError>)> {
    // Verify item exists and is viewable by this user (FR-8 Story 3.3): comments
    // on an item the viewer cannot see must not leak. Denied access returns 404
    // (same as missing) so item existence is not disclosed.
    let item = state.items().load(item_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load item");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "Internal server error".to_string(),
            }),
        )
    })?;

    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Item not found".to_string(),
            }),
        )
    };

    let Some(item) = item else {
        return Err(not_found());
    };
    let user = crate::routes::item::get_user_context(&session, &state).await;
    if !state
        .items()
        .check_access(&item, "view", &user)
        .await
        .unwrap_or(false)
    {
        return Err(not_found());
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
    // One loader, one context shape: the same one every read handler in this
    // module gets from `get_user_context`, reached from an already-loaded user.
    let user_ctx = user_context_for(&state, &user).await;

    // Check "post comments" permission
    if !user_ctx.is_admin() && !user_ctx.has_permission("post comments") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(JsonError {
                error: "You do not have permission to post comments".to_string(),
            }),
        ));
    }

    // The status a new comment gets: the site's default, unless this commenter
    // is trusted enough to bypass the queue. `skip comment approval` is declared
    // by `trovato_comments` and, before this, was read by nothing.
    let default_status = CommentStatus::default_for_new_comments(state.db()).await;
    let status = if default_status.awaits_review()
        && (user_ctx.is_admin() || user_ctx.has_permission("skip comment approval"))
    {
        CommentStatus::Published
    } else {
        default_status
    };

    // Create comment
    let input = CreateComment {
        item_id,
        parent_id: request.parent_id,
        author_id: user_id,
        body: request.body.clone(),
        body_format: Some("filtered_html".to_string()),
        status: Some(status.as_i16()),
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

    // Notify the content author, but only if this comment is visible. A comment
    // created into the review queue must not mail its full text to the author:
    // that would deliver every held comment, including the ones moderation
    // exists to catch.
    notify_if_published(
        &state,
        &comment,
        None,
        &item,
        commenter.as_ref().map(|a| a.name.as_str()),
    );

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
    session: Session,
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

    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(JsonError {
                error: "Comment not found".to_string(),
            }),
        )
    };

    let comment = comment.ok_or_else(not_found)?;

    // Enforce parent-item access (FR-8 Story 3.3): a comment on an inaccessible
    // item is not exposed. Denied (or missing) parent ⇒ 404, no existence leak.
    let user = crate::routes::item::get_user_context(&session, &state).await;
    match state.items().load(comment.item_id).await {
        Ok(Some(item))
            if state
                .items()
                .check_access(&item, "view", &user)
                .await
                .unwrap_or(false) => {}
        _ => return Err(not_found()),
    }

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
    // One loader, one context shape: the same one every read handler in this
    // module gets from `get_user_context`, reached from an already-loaded user.
    let user_ctx = user_context_for(&state, &user).await;

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
    // One loader, one context shape: the same one every read handler in this
    // module gets from `get_user_context`, reached from an already-loaded user.
    let user_ctx = user_context_for(&state, &user).await;

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

/// Whether a status change should mail the content author.
///
/// The rule is "on becoming visible", not "on being created". A comment created
/// straight to published notifies (`previous` is `None`); a comment approved out
/// of the review queue notifies; re-saving an already published comment does
/// not, so an edit or a re-approval cannot mail the author twice; and a comment
/// entering any non-visible status — held, hidden, marked spam — never notifies.
///
/// The author commenting on their own content never notifies either, which is
/// the one part of this rule that predates moderation.
pub(crate) fn should_notify_on_publish(
    previous: Option<i16>,
    new: i16,
    comment_author: uuid::Uuid,
    item_author: uuid::Uuid,
) -> bool {
    let becomes_visible = CommentStatus::from_i16(new).is_some_and(CommentStatus::is_visible);
    let was_visible = previous
        .and_then(CommentStatus::from_i16)
        .is_some_and(CommentStatus::is_visible);

    becomes_visible && !was_visible && comment_author != item_author
}

/// Mail the content author if this comment has just become visible.
///
/// `previous` is the status before the change, or `None` for a comment that has
/// just been created. Spawns the send, so a slow or dead SMTP server cannot hold
/// up the response.
pub(crate) fn notify_if_published(
    state: &AppState,
    comment: &Comment,
    previous: Option<i16>,
    item: &crate::models::Item,
    commenter_name: Option<&str>,
) {
    if !should_notify_on_publish(previous, comment.status, comment.author_id, item.author_id) {
        return;
    }

    let Some(email_service) = state.email() else {
        return;
    };

    let notification_state = state.clone();
    let email = email_service.clone();
    let comment_body = comment.body.clone();
    let item_title = item.title.clone();
    let item_author_id = item.author_id;
    let item_id = item.id;
    let commenter_name = commenter_name.unwrap_or("Someone").to_string();

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

    // Truncate comment preview for email. Walks back to a char boundary: a
    // multi-byte character straddling byte 500 would panic a bare slice, and
    // this runs in a spawned task where that takes the task down silently.
    let preview: &str = if comment_text.len() > 500 {
        let mut end = 500;
        while end > 0 && !comment_text.is_char_boundary(end) {
            end -= 1;
        }
        &comment_text[..end]
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

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const UNPUBLISHED: i16 = 0;
    const PUBLISHED: i16 = 1;
    const PENDING: i16 = 2;
    const SPAM: i16 = 3;

    fn commenter() -> Uuid {
        Uuid::from_u128(1)
    }

    fn item_author() -> Uuid {
        Uuid::from_u128(2)
    }

    /// The behaviour the defect was about: a comment created into the review
    /// queue must not mail the content author. Under a hold-for-review default
    /// the old code mailed the full text of every held comment, including the
    /// ones moderation exists to catch.
    #[test]
    fn a_comment_created_into_the_queue_does_not_notify() {
        for status in [PENDING, SPAM, UNPUBLISHED] {
            assert!(
                !should_notify_on_publish(None, status, commenter(), item_author()),
                "status {status} must not notify on create"
            );
        }
    }

    /// A site that publishes immediately still notifies on create, which is what
    /// it did before.
    #[test]
    fn a_comment_created_published_notifies() {
        assert!(should_notify_on_publish(
            None,
            PUBLISHED,
            commenter(),
            item_author()
        ));
    }

    /// Approving out of the queue is where the mail belongs.
    #[test]
    fn approving_a_held_comment_notifies() {
        for previous in [PENDING, SPAM, UNPUBLISHED] {
            assert!(
                should_notify_on_publish(Some(previous), PUBLISHED, commenter(), item_author()),
                "{previous} -> published must notify"
            );
        }
    }

    /// Re-saving an already published comment must not mail again, so an edit or
    /// a second approval cannot double-notify.
    #[test]
    fn re_publishing_an_already_published_comment_does_not_notify() {
        assert!(!should_notify_on_publish(
            Some(PUBLISHED),
            PUBLISHED,
            commenter(),
            item_author()
        ));
    }

    /// Leaving published never notifies either.
    #[test]
    fn hiding_a_comment_does_not_notify() {
        for status in [UNPUBLISHED, PENDING, SPAM] {
            assert!(
                !should_notify_on_publish(Some(PUBLISHED), status, commenter(), item_author()),
                "published -> {status} must not notify"
            );
        }
    }

    /// Predates moderation and still holds: nobody is told about their own
    /// comment on their own content.
    #[test]
    fn the_content_author_commenting_on_their_own_item_does_not_notify() {
        assert!(!should_notify_on_publish(
            None,
            PUBLISHED,
            item_author(),
            item_author()
        ));
        assert!(!should_notify_on_publish(
            Some(PENDING),
            PUBLISHED,
            item_author(),
            item_author()
        ));
    }

    /// A status this build does not know is not visible, so it cannot notify —
    /// the same fail-closed reading the read paths use.
    #[test]
    fn an_unknown_status_does_not_notify() {
        assert!(!should_notify_on_publish(
            None,
            99,
            commenter(),
            item_author()
        ));
        assert!(!should_notify_on_publish(
            Some(99),
            99,
            commenter(),
            item_author()
        ));
        // Unknown -> published is a transition into visible, and does notify.
        assert!(should_notify_on_publish(
            Some(99),
            PUBLISHED,
            commenter(),
            item_author()
        ));
    }
}
