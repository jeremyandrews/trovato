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

/// One comment submission, however it arrived.
///
/// The comment form in `elements/comments.html` is an HTML `<form>`, and an HTML
/// form posts `application/x-www-form-urlencoded` with no way to set a header —
/// so posting the form the kernel renders was a 415 by construction, and its
/// CSRF token had nowhere to travel. This route now accepts both encodings and
/// normalizes them here, the same shape
/// [`routes::item::ItemSubmission`](crate::routes::item) uses for the item form.
///
/// JSON requests are unchanged, including where their CSRF token comes from:
///
/// - `application/json` → the existing body; CSRF from the `X-CSRF-Token`
///   header.
/// - `application/x-www-form-urlencoded` → CSRF from the `_csrf` hidden input,
///   and the response is a redirect rather than JSON, because the caller is a
///   browser following a form submission.
#[derive(Debug, Default)]
pub struct CommentSubmission {
    /// Comment text.
    pub body: String,
    /// Parent comment, for a threaded reply.
    pub parent_id: Option<Uuid>,
    /// CSRF token from the form body; `None` when the request is JSON and the
    /// token belongs in the header instead.
    pub body_csrf: Option<String>,
    /// Whether the caller is a browser posting a form, and so wants a redirect.
    pub wants_redirect: bool,
}

impl CommentSubmission {
    /// Verify this submission's CSRF token from whichever place it travels.
    async fn verify_csrf(
        &self,
        session: &Session,
        headers: &HeaderMap,
    ) -> Result<(), (StatusCode, Json<JsonError>)> {
        let csrf_error = |s: StatusCode| {
            (
                s,
                Json(JsonError {
                    error: "Invalid or missing CSRF token".to_string(),
                }),
            )
        };

        match self.body_csrf {
            Some(ref token) => crate::routes::helpers::require_csrf(session, token)
                .await
                .map(|_| ())
                .map_err(|_| csrf_error(StatusCode::FORBIDDEN)),
            None => require_csrf_header(session, headers)
                .await
                .map_err(|(s, _)| csrf_error(s)),
        }
    }

    /// Build a submission from a decoded urlencoded form body.
    fn from_form(form: std::collections::HashMap<String, String>) -> Self {
        Self {
            body: form.get("body").cloned().unwrap_or_default(),
            // An empty hidden input posts "", which is "no parent" rather than a
            // malformed UUID.
            parent_id: form
                .get("parent_id")
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .and_then(|v| v.parse().ok()),
            body_csrf: Some(form.get("_csrf").cloned().unwrap_or_default()),
            wants_redirect: true,
        }
    }
}

impl<S: Send + Sync> axum::extract::FromRequest<S> for CommentSubmission {
    type Rejection = (StatusCode, Json<JsonError>);

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        let bad_request =
            |message: String| (StatusCode::BAD_REQUEST, Json(JsonError { error: message }));

        if content_type.starts_with("application/x-www-form-urlencoded") {
            let axum::Form(form) =
                axum::Form::<std::collections::HashMap<String, String>>::from_request(req, state)
                    .await
                    .map_err(|e| bad_request(format!("invalid form body: {e}")))?;
            return Ok(Self::from_form(form));
        }

        // Everything else keeps the pre-existing JSON behaviour.
        let Json(request) = Json::<CreateCommentRequest>::from_request(req, state)
            .await
            .map_err(|e| bad_request(format!("invalid JSON body: {e}")))?;
        Ok(Self {
            body: request.body,
            parent_id: request.parent_id,
            body_csrf: None,
            wants_redirect: false,
        })
    }
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
    submission: CommentSubmission,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let wants_redirect = submission.wants_redirect;
    let result = create_comment_inner(&state, &session, &headers, item_id, submission).await;

    if !wants_redirect {
        // JSON in, JSON out: byte-for-byte what an API client got before.
        return match result {
            Ok(response) => Json(response).into_response(),
            Err(error) => error.into_response(),
        };
    }

    // A browser posting a form gets a redirect back to the item, so the page it
    // lands on is the page it came from and a reload does not repost.
    let target = item_path_for(&state, item_id).await;
    let outcome = match &result {
        Ok(response) => {
            if crate::models::CommentStatus::from_i16(response.status)
                .is_some_and(|s| s.awaits_review())
            {
                "pending"
            } else {
                "posted"
            }
        }
        Err(_) => "error",
    };

    if let Err((status, message)) = &result {
        tracing::debug!(status = %status, error = %message.0.error, "comment form submission refused");
    }

    axum::response::Redirect::to(&format!("{target}?comment={outcome}#comments")).into_response()
}

/// The item's own address, alias included when it has one, for the post-submit
/// redirect.
async fn item_path_for(state: &AppState, item_id: Uuid) -> String {
    let source = format!("/item/{item_id}");
    crate::models::UrlAlias::get_canonical_alias(state.db(), &source)
        .await
        .ok()
        .flatten()
        .unwrap_or(source)
}

/// Create a comment. Shared by both request encodings.
async fn create_comment_inner(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    item_id: Uuid,
    request: CommentSubmission,
) -> Result<CommentResponse, (StatusCode, Json<JsonError>)> {
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

    // Verify the CSRF token from wherever this submission carries it.
    request.verify_csrf(session, headers).await?;

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
    let user_ctx = user_context_for(state, &user).await;

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
        state,
        &comment,
        None,
        &item,
        commenter.as_ref().map(|a| a.name.as_str()),
    );

    let body_html = render_comment_body(&comment);

    Ok(CommentResponse {
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
    })
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
// Front-end rendering
// =============================================================================

/// Render the comment thread and form for an item page.
///
/// Returns an empty string when there is nothing to show: comments disabled, or
/// a template that failed to render. An item page must not fail because its
/// comment section did.
///
/// `elements/comments.html` existed before this and was rendered by nothing —
/// the only comment template any route used was the admin one. It is resolved
/// through the theme engine, so a theme can override it.
pub(crate) async fn render_thread(
    state: &AppState,
    session: &Session,
    item: &crate::models::Item,
    viewer: &crate::tap::UserContext,
    current_path: &str,
    outcome: Option<&str>,
) -> String {
    let Some(comments) = state.comments_if_enabled() else {
        return String::new();
    };

    let thread = match comments.list_for_item(item.id).await {
        Ok(thread) => thread,
        Err(e) => {
            tracing::warn!(item_id = %item.id, error = %e, "failed to load comments for item page");
            return String::new();
        }
    };

    // Author names, one lookup per distinct author rather than per comment.
    let mut names: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();
    for comment in &thread {
        if !names.contains_key(&comment.author_id)
            && let Ok(Some(user)) = state.users().find_by_id(comment.author_id).await
        {
            names.insert(comment.author_id, user.name);
        }
    }

    let rendered: Vec<serde_json::Value> = thread
        .iter()
        .map(|comment| {
            serde_json::json!({
                "id": comment.id,
                "depth": comment.depth,
                "created": comment.created,
                "author_name": names.get(&comment.author_id),
                "body_html": render_comment_body(comment),
            })
        })
        .collect();

    // Posting is what the create route will actually allow, asked the same way.
    let can_comment =
        viewer.authenticated && (viewer.is_admin() || viewer.has_permission("post comments"));

    let mut context = tera::Context::new();
    context.insert("comments", &rendered);
    context.insert("total", &rendered.len());
    context.insert("item_id", &item.id);
    context.insert("can_comment", &can_comment);
    context.insert("can_reply", &can_comment);
    context.insert("user_logged_in", &viewer.authenticated);
    context.insert("current_path", &current_path);
    context.insert("comment_outcome", &outcome);
    if can_comment {
        // The form posts this in `_csrf`; the JS layer reads the meta tag in the
        // head instead. Only generated for a viewer who can post.
        let token = crate::form::csrf::generate_csrf_token(session).await;
        context.insert("csrf_token", &token);
    }

    let template = state
        .theme()
        .resolve_template(&["elements/comments"])
        .unwrap_or_else(|| "elements/comments.html".to_string());

    state
        .theme()
        .tera()
        .render(&template, &context)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to render the comment thread");
            String::new()
        })
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

    fn form(pairs: &[(&str, &str)]) -> CommentSubmission {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        CommentSubmission::from_form(map)
    }

    /// A form submission carries its CSRF token in the body, because an HTML form
    /// cannot set a header. That is why posting the rendered form used to fail.
    #[test]
    fn a_form_submission_takes_its_csrf_token_from_the_body() {
        let submission = form(&[("body", "Hello"), ("_csrf", "token-value")]);

        assert_eq!(submission.body, "Hello");
        assert_eq!(submission.body_csrf.as_deref(), Some("token-value"));
        assert!(
            submission.wants_redirect,
            "a browser posting a form wants a page, not JSON"
        );
    }

    /// The reply field is an empty hidden input on a top-level comment, and ""
    /// means "no parent" rather than a malformed UUID.
    #[test]
    fn an_empty_parent_id_field_is_no_parent() {
        for value in ["", "   "] {
            let submission = form(&[("body", "Hello"), ("parent_id", value)]);
            assert_eq!(
                submission.parent_id, None,
                "{value:?} must not be read as a parent"
            );
        }
    }

    #[test]
    fn a_parent_id_field_is_read_when_it_holds_one() {
        let parent = Uuid::now_v7();
        let submission = form(&[("body", "Hello"), ("parent_id", &parent.to_string())]);

        assert_eq!(submission.parent_id, Some(parent));
    }

    /// A missing `_csrf` still routes to the body check rather than falling
    /// through to the header one, so a form post cannot skip CSRF by omitting the
    /// field.
    #[test]
    fn a_form_submission_without_a_token_still_checks_the_body() {
        let submission = form(&[("body", "Hello")]);

        assert_eq!(submission.body_csrf.as_deref(), Some(""));
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
