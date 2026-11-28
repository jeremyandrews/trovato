//! Comment routes for threaded discussions.
//!
//! Provides endpoints for viewing, creating, and moderating comments on content items.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::FilterPipeline;
use crate::error::AppError;
use crate::models::comment::{SUBJECT_MAX_CHARS, normalize_subject, subject_too_long};
use crate::models::{Comment, CommentStatus, CreateComment, UpdateComment};
use crate::pagination::{Page, PageParams};
use crate::routes::helpers::{require_csrf_header, require_login_json, user_context_for};
use crate::state::AppState;

/// The text format a comment posted through the comment routes is stored in,
/// and so the one its preview is rendered through.
const NEW_COMMENT_FORMAT: &str = "filtered_html";

/// Render a comment body to HTML with safe format whitelisting.
fn render_comment_body(comment: &Comment) -> String {
    FilterPipeline::for_format(&comment.body_format).process(&comment.body)
}

/// Normalize and length-check a submitted subject, or the 400 it earns.
///
/// Shared by every route that accepts one (the JSON API here and the
/// moderation form in [`super::admin`]), so the limit and the "empty means no
/// subject" rule cannot drift between them.
pub(crate) fn validate_subject(raw: Option<&str>) -> Result<Option<String>, AppError> {
    let subject = normalize_subject(raw);
    if let Some(ref s) = subject
        && subject_too_long(s)
    {
        return Err(AppError::with_status(
            StatusCode::BAD_REQUEST,
            format!("Comment subject cannot exceed {SUBJECT_MAX_CHARS} characters"),
        ));
    }
    Ok(subject)
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
    /// Absent when the comment has none, so a client that never sends a
    /// subject never sees the key at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
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

// =============================================================================
// Request Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateCommentRequest {
    pub body: String,
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub subject: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCommentRequest {
    pub body: Option<String>,
    pub status: Option<i16>,
    /// Absent (no `subject` key at all) means unchanged; present — including
    /// an empty string — replaces it, the same shape [`UpdateComment::subject`]
    /// takes once this is validated.
    #[serde(default)]
    pub subject: Option<String>,
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
/// - `application/x-www-form-urlencoded` → the form `comment_form`, checked
///   through the forms pipeline (the `_token` hidden input, its saved state,
///   `tap_form_validate`), and the response is a redirect rather than JSON,
///   because the caller is a browser following a form submission.
#[derive(Debug, Default)]
pub struct CommentSubmission {
    /// Comment text.
    pub body: String,
    /// Optional plain text subject, from whichever encoding carried it.
    pub subject: Option<String>,
    /// Parent comment, for a threaded reply.
    pub parent_id: Option<Uuid>,
    /// Every posted field of a form submission, for the forms pipeline;
    /// `None` when the request is JSON and its token is in the header.
    pub posted: Option<crate::content::submission::PostedForm>,
    /// Whether the caller is a browser posting a form, and so wants a redirect.
    pub wants_redirect: bool,
}

impl CommentSubmission {
    /// Build a submission from a decoded urlencoded form body.
    fn from_form(pairs: Vec<(String, String)>) -> Self {
        let posted = crate::content::submission::PostedForm(pairs);
        Self {
            body: posted.get("body").unwrap_or_default().to_string(),
            subject: posted.get("subject").map(str::to_string),
            // An empty hidden input posts "", which is "no parent" rather than a
            // malformed UUID.
            parent_id: posted
                .get("parent_id")
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .and_then(|v| v.parse().ok()),
            posted: Some(posted),
            wants_redirect: true,
        }
    }
}

impl<S: Send + Sync> axum::extract::FromRequest<S> for CommentSubmission {
    type Rejection = AppError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        let bad_request = |message: String| AppError::with_status(StatusCode::BAD_REQUEST, message);

        if content_type.starts_with("application/x-www-form-urlencoded") {
            let axum::Form(pairs) = axum::Form::<Vec<(String, String)>>::from_request(req, state)
                .await
                .map_err(|e| bad_request(format!("invalid form body: {e}")))?;
            return Ok(Self::from_form(pairs));
        }

        // Everything else keeps the pre-existing JSON behaviour.
        let Json(request) = Json::<CreateCommentRequest>::from_request(req, state)
            .await
            .map_err(|e| bad_request(format!("invalid JSON body: {e}")))?;
        Ok(Self {
            body: request.body,
            subject: request.subject,
            parent_id: request.parent_id,
            posted: None,
            wants_redirect: false,
        })
    }
}

/// What a comment form shows: the subject and body to fill in, and on the
/// item page the reply controls `comment-reply.js` and `comment-post.js` use.
#[derive(Debug, Default)]
struct CommentFormValues<'a> {
    subject: &'a str,
    body: &'a str,
}

/// The comment form's Preview button, and the field a click on it posts.
const PREVIEW_BUTTON: &str = "preview";

/// The form a new comment or reply is posted with, `comment_form`: under the
/// thread on the item page (`inline`), and on the reply page.
fn build_comment_form(
    item_id: Uuid,
    parent_id: Option<Uuid>,
    values: CommentFormValues<'_>,
    inline: bool,
    item_path: &str,
) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    let mut form = Form::new("comment_form")
        .action(format!("/api/v1/item/{item_id}/comments"))
        .attribute("class", "comment-form")
        .element(
            "parent_id",
            FormElement::hidden()
                .default_value(parent_id.map(|p| p.to_string()).unwrap_or_default()),
        )
        .element(
            "subject",
            FormElement::textfield()
                .title("Subject (optional)")
                .max_length(crate::models::comment::SUBJECT_MAX_CHARS)
                .default_value(values.subject)
                .weight(0),
        );
    let body = FormElement::textarea(if inline { 4 } else { 8 })
        .required()
        .default_value(values.body)
        .weight(1);
    // Preview posts the form like the other button; `create_comment` sees
    // `preview` among the posted fields and shows the comment instead of
    // saving it. `comment-post.js` leaves this button's submission alone.
    form = form.element(PREVIEW_BUTTON, FormElement::submit("Preview").weight(99));
    if inline {
        // The form `comment-post.js` posts as JSON and `comment-reply.js`
        // points at a comment.
        form = form
            .attribute("data-comment-form", "")
            .element(
                "body",
                body.title("Your comment")
                    .placeholder("Write your comment..."),
            )
            .element("op", FormElement::submit("Post Comment").weight(100))
            .element(
                "replying",
                FormElement::markup(
                    r#"<span id="replying-to" class="replying-indicator" hidden>Replying to comment <span id="reply-parent-id"></span> <button type="button" class="cancel-reply">Cancel</button></span>"#,
                )
                .weight(101),
            )
            .element(
                "status",
                FormElement::markup(
                    r#"<p class="comment-form__status" data-comment-status role="status" aria-live="polite"></p>"#,
                )
                .weight(102),
            );
    } else {
        form = form.element("body", body.title("Comment")).element(
            "op",
            FormElement::submit("Save")
                .suffix(cancel_link(&format!("{item_path}#comments")))
                .weight(100),
        );
    }
    form
}

/// The Cancel link beside a comment page's button, back to `href`.
fn cancel_link(href: &str) -> String {
    format!(
        r#"<a href="{}" class="button">Cancel</a>"#,
        crate::routes::helpers::html_escape(href)
    )
}

#[derive(Debug, Deserialize)]
pub struct ListCommentsQuery {
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
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<CommentResponse>>, AppError> {
    // Verify item exists and is viewable by this user (FR-8 Story 3.3): comments
    // on an item the viewer cannot see must not leak. Denied access returns 404
    // (same as missing) so item existence is not disclosed.
    let item = state.items().load(item_id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load item");
        AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;

    let not_found = || AppError::with_status(StatusCode::NOT_FOUND, "Item not found");

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
        AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;

    // The window of the thread, in thread order.
    let page = Page::slice(comments, paging.window(50, 100));

    // Build response with optional author info
    let mut comment_responses = Vec::with_capacity(page.data.len());
    let mut author_cache: std::collections::HashMap<Uuid, AuthorInfo> =
        std::collections::HashMap::new();

    let (total, window) = (page.total, (page.page, page.per_page));
    for comment in page.data {
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
            subject: comment.subject,
            body: comment.body,
            body_html,
            status: comment.status,
            created: comment.created,
            changed: comment.changed,
            depth: comment.depth,
        });
    }

    Ok(Json(Page {
        data: comment_responses,
        total,
        page: window.0,
        per_page: window.1,
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
    mut submission: CommentSubmission,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let wants_redirect = submission.wants_redirect;
    // A form submission goes through the forms pipeline first: its CSRF
    // token, its saved state and `tap_form_validate`. A refusal is the reply
    // form again, with the errors and what was written.
    let posted = submission.posted.take();
    let checked = match &posted {
        Some(posted) => {
            let viewer = crate::routes::item::get_user_context(&session, &state).await;
            match super::admin_form::check_as(
                &state,
                &session,
                &viewer,
                posted,
                "comment_form",
                Vec::new(),
            )
            .await
            {
                // Preview: the comment as it would be posted, above the form
                // still holding it. Nothing is written.
                Ok(_) if posted.get(PREVIEW_BUTTON).is_some() => {
                    return preview_page(&state, &session, &viewer, item_id, &submission).await;
                }
                Ok(form_state) => Some(Ok((form_state, viewer))),
                Err(Ok(errors)) => {
                    let values = CommentFormValues {
                        subject: submission.subject.as_deref().unwrap_or_default(),
                        body: &submission.body,
                    };
                    return reply_page(
                        &state,
                        &session,
                        item_id,
                        submission.parent_id,
                        values,
                        &errors,
                        None,
                    )
                    .await;
                }
                Err(Err(_)) => Some(Err(AppError::forbidden("Invalid or missing CSRF token"))),
            }
        }
        None => None,
    };
    let result = match checked {
        Some(Err(refusal)) => Err(refusal),
        Some(Ok((form_state, viewer))) => {
            let result =
                create_comment_inner(&state, &session, &headers, item_id, submission, true).await;
            if result.is_ok()
                && let Some(posted) = &posted
            {
                state
                    .forms()
                    .submitted(
                        &form_state,
                        posted,
                        None,
                        &super::item_form::tap_state(&state, &viewer),
                    )
                    .await;
            }
            result
        }
        None => create_comment_inner(&state, &session, &headers, item_id, submission, false).await,
    };

    if !wants_redirect {
        // JSON in, JSON out: byte-for-byte what an API client got before.
        return match result {
            Ok(response) => Json(response).into_response(),
            Err(error) => error.into_response(),
        };
    }

    // A browser posting a form gets a redirect back to the item, so the page it
    // lands on is the page it came from and a reload does not repost.
    let posted = match &result {
        Ok(response) => state.comments().load(response.id).await.ok().flatten(),
        Err(_) => None,
    };
    let (target, anchor) = match &posted {
        // The page of the thread the new comment is on, at the comment.
        Some(comment) if comment.status == CommentStatus::Published.as_i16() => (
            comment_location(&state, comment).await,
            format!("comment-{}", comment.id),
        ),
        _ => (item_path_for(&state, item_id).await, "comments".to_string()),
    };
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

    if let Err(refusal) = &result {
        tracing::debug!(status = %refusal.status(), error = %refusal.public_message(), "comment form submission refused");
    }

    axum::response::Redirect::to(&format!(
        "{}#{anchor}",
        with_query(&target, "comment", outcome)
    ))
    .into_response()
}

/// The reply page showing `submission` as it would look once posted: the
/// subject and body through the text format a posted comment is stored in, in
/// the comment pages' own markup, above the form still holding what was
/// typed. Nothing is saved; the reply page asks every question posting would
/// (signed in, the item visible, `post comments`, comments open).
async fn preview_page(
    state: &AppState,
    session: &Session,
    viewer: &crate::tap::UserContext,
    item_id: Uuid,
    submission: &CommentSubmission,
) -> axum::response::Response {
    let values = CommentFormValues {
        subject: submission.subject.as_deref().unwrap_or_default(),
        body: &submission.body,
    };
    let (preview, errors) = match validate_subject(submission.subject.as_deref()) {
        Ok(subject) => (
            Some(serde_json::json!({
                "author_name": viewer.name(),
                "created": chrono::Utc::now().timestamp(),
                "subject": subject,
                "body_html": FilterPipeline::for_format(NEW_COMMENT_FORMAT).process(&submission.body),
            })),
            Vec::new(),
        ),
        Err(refusal) => (
            None,
            vec![crate::form::ValidationError::field(
                "subject",
                refusal.public_message(),
            )],
        ),
    };
    reply_page(
        state,
        session,
        item_id,
        submission.parent_id,
        values,
        &errors,
        preview,
    )
    .await
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
    form_checked: bool,
) -> Result<CommentResponse, AppError> {
    // Check authentication
    let user = require_login_json(state, session).await?;
    let user_id = user.id;

    // A JSON request's CSRF token is in its header. A form submission's was
    // checked with the rest of the form (`form_checked`) before this.
    if !form_checked {
        require_csrf_header(session, headers).await?;
    }

    // Verify item exists (used for notification below)
    let item = state
        .items()
        .load(item_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to load item");
            AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "Item not found"))?;

    // And the commenter has to be able to *see* the item. `post comments` used
    // to be the whole rule here, so it was also a way to learn that a draft
    // exists: the refusal is the one a missing item produces, word for word.
    let user_ctx = crate::routes::item::get_user_context(session, state).await;
    if !state
        .items()
        .check_access(&item, "view", &user_ctx)
        .await
        .unwrap_or(false)
    {
        return Err(AppError::with_status(
            StatusCode::NOT_FOUND,
            "Item not found",
        ));
    }

    // Only an item whose type takes comments. Closed shows the thread and
    // takes nothing new; hidden shows nothing. The form is not offered on
    // either, and this is what stops a post that does not come from it.
    if settings_for_type(state, &item.item_type).await.mode
        != crate::content::comment_settings::CommentMode::Open
    {
        return Err(AppError::with_status(
            StatusCode::FORBIDDEN,
            "Comments are closed for this item",
        ));
    }

    // Verify parent comment exists if specified
    if let Some(parent_id) = request.parent_id {
        let parent = state.comments().load(parent_id).await.map_err(|e| {
            tracing::error!(error = %e, "failed to load parent comment");
            AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?;

        let Some(parent) = parent else {
            return Err(AppError::with_status(
                StatusCode::BAD_REQUEST,
                "Parent comment not found",
            ));
        };

        // Verify parent is on the same item
        if parent.item_id != item_id {
            return Err(AppError::with_status(
                StatusCode::BAD_REQUEST,
                "Parent comment is on a different item",
            ));
        }
    }

    // Validate body
    if request.body.trim().is_empty() {
        return Err(AppError::with_status(
            StatusCode::BAD_REQUEST,
            "Comment body cannot be empty",
        ));
    }

    let subject = validate_subject(request.subject.as_deref())?;

    // Build UserContext with real permissions for access check
    // One loader, one context shape: the same one every read handler in this
    // module gets from `get_user_context`, reached from an already-loaded user.
    let user_ctx = user_context_for(state, &user).await;

    // Check "post comments" permission
    if !user_ctx.is_admin() && !user_ctx.has_permission("post comments") {
        return Err(AppError::with_status(
            StatusCode::FORBIDDEN,
            "You do not have permission to post comments",
        ));
    }

    // The status a new comment gets: the site's default, unless this commenter is
    // excused from the queue. Two ways to be excused. `skip comment approval` is
    // the explicit grant — declared by `trovato_comments` and, before this, read by
    // nothing. The trust ladder is the earned one: an author with enough approved
    // comments skips the wait, while the classifier still runs and can take the
    // comment down afterwards.
    let default_status = CommentStatus::default_for_new_comments(state.db()).await;
    let may_skip_approval = user_ctx.is_admin() || user_ctx.has_permission("skip comment approval");

    // Only asked when it can change the answer: a site that publishes immediately,
    // or a commenter already excused, needs no count.
    let (approved, threshold) = if default_status.awaits_review() && !may_skip_approval {
        let threshold = CommentStatus::trust_threshold(state.db()).await;
        let approved = if threshold.is_some() {
            Comment::approved_count_for_author(state.db(), user_id)
                .await
                .unwrap_or(0)
        } else {
            0
        };
        (approved, threshold)
    } else {
        (0, None)
    };

    let status =
        CommentStatus::for_new_comment(default_status, may_skip_approval, approved, threshold);

    // Create comment
    let input = CreateComment {
        item_id,
        parent_id: request.parent_id,
        author_id: user_id,
        subject,
        body: request.body.clone(),
        body_format: Some(NEW_COMMENT_FORMAT.to_string()),
        status: Some(status.as_i16()),
    };
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
    // exists to catch. The notification is queued in the comment's own
    // transaction (BG18).
    let mail_ctx = if should_notify_on_publish(None, status.as_i16(), user_id, item.author_id) {
        comment_mail_context(state, &item, commenter.as_ref().map(|a| a.name.as_str())).await
    } else {
        None
    };
    let created = match (state.email(), mail_ctx) {
        (Some(email), Some(ctx)) => {
            let tera = state.theme().tera();
            state
                .comments()
                .create_with_mail(input, &user_ctx, email, |c| {
                    comment_notification(tera, &ctx, c, None)
                })
                .await
        }
        _ => state.comments().create(input, &user_ctx).await,
    };
    let comment = created.map_err(|e| {
        tracing::error!(error = %e, "failed to create comment");
        AppError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create comment",
        )
    })?;

    let body_html = render_comment_body(&comment);

    Ok(CommentResponse {
        id: comment.id,
        item_id: comment.item_id,
        parent_id: comment.parent_id,
        author_id: comment.author_id,
        author: commenter,
        subject: comment.subject,
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
) -> Result<Json<CommentResponse>, AppError> {
    let comment = state.comments().load(id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;

    let not_found = || AppError::with_status(StatusCode::NOT_FOUND, "Comment not found");

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

    // And the comment's own status. `Comment::list_for_item` filters on it, so
    // the listing never showed a held, unpublished or spam comment; this
    // endpoint returned one to anyone who could see the parent item, one id at
    // a time. The three exceptions are the ones that already exist elsewhere:
    // its author (who has to see what they posted while it waits), an
    // administrator, and a holder of the moderation permission.
    if comment.status != CommentStatus::Published.as_i16() {
        let is_author = user.authenticated && user.id == comment.author_id;
        if !is_author && !user.can("administer comments") {
            return Err(not_found());
        }
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
        subject: comment.subject,
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
) -> Result<Json<CommentResponse>, AppError> {
    // Check authentication
    let user = require_login_json(&state, &session).await?;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers).await?;

    // Load existing comment
    let existing = state.comments().load(id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;

    let existing = existing
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "Comment not found"))?;

    // Build UserContext for the acting user to check access
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
            AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?;
    if !has_access {
        return Err(AppError::with_status(
            StatusCode::FORBIDDEN,
            "You do not have permission to edit this comment",
        ));
    }

    // Validate body if provided
    if let Some(ref body) = request.body
        && body.trim().is_empty()
    {
        return Err(AppError::with_status(
            StatusCode::BAD_REQUEST,
            "Comment body cannot be empty",
        ));
    }

    // `request.subject` is `None` when the JSON carried no `subject` key at
    // all (leave it alone); `Some(raw)` is validated and becomes the new
    // value, including `Some(None)` when `raw` was empty (clear it).
    let subject = match request.subject.as_deref() {
        None => None,
        Some(raw) => Some(validate_subject(Some(raw))?),
    };

    let input = UpdateComment {
        body: request.body,
        body_format: None,
        status: request.status,
        subject,
    };

    let comment = state
        .comments()
        .update(id, input, &user_ctx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to update comment");
            AppError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to update comment",
            )
        })?
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "Comment not found"))?;

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
        subject: comment.subject,
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
) -> Result<Json<serde_json::Value>, AppError> {
    // Check authentication
    let user = require_login_json(&state, &session).await?;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers).await?;

    // Load existing comment
    let existing = state.comments().load(id).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load comment");
        AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;

    let existing = existing
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "Comment not found"))?;

    // Build UserContext for access check
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
            AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?;
    if !has_access {
        return Err(AppError::with_status(
            StatusCode::FORBIDDEN,
            "You do not have permission to delete this comment",
        ));
    }

    state.comments().delete(id, &user_ctx).await.map_err(|e| {
        tracing::error!(error = %e, "failed to delete comment");
        AppError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete comment",
        )
    })?;

    Ok(Json(serde_json::json!({"deleted": true})))
}

// =============================================================================
// Front-end rendering
// =============================================================================

/// A content type's comment settings: one query, per page rather than per
/// comment.
pub(crate) async fn settings_for_type(
    state: &AppState,
    item_type: &str,
) -> crate::content::comment_settings::CommentSettings {
    let row = crate::models::ItemType::find_by_type(state.db(), item_type)
        .await
        .ok()
        .flatten();
    crate::content::comment_settings::CommentSettings::of(row.as_ref().map(|t| &t.settings))
}

/// An item's published comments in the order its type shows them: threaded
/// (each reply under its parent) or flat (by date), oldest or newest first.
async fn ordered_thread(
    comments: &crate::services::comment::CommentService,
    item_id: Uuid,
    settings: crate::content::comment_settings::CommentSettings,
) -> anyhow::Result<Vec<Comment>> {
    let mut thread = comments
        .list_for_item_ordered(item_id, settings.newest_first)
        .await?;
    if !settings.threaded {
        thread.sort_by_key(|c| (c.created, c.id));
        if settings.newest_first {
            thread.reverse();
        }
    }
    Ok(thread)
}

/// Add `key=value` to a path that may already carry a query string.
fn with_query(path: &str, key: &str, value: &str) -> String {
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}{key}={value}")
}

/// Where a comment is read: its item's address, on the page of the thread
/// that holds it (`?page=N` from the second page on).
///
/// For the redirect after a comment is posted or edited, so the writer lands
/// on their comment and not on the first page of a long thread.
pub(crate) async fn comment_location(state: &AppState, comment: &Comment) -> String {
    let item_path = item_path_for(state, comment.item_id).await;
    let (Some(comments), Ok(Some(item))) = (
        state.comments_if_enabled(),
        state.items().load(comment.item_id).await,
    ) else {
        return item_path;
    };
    let settings = settings_for_type(state, &item.item_type).await;
    let page = ordered_thread(comments, item.id, settings)
        .await
        .ok()
        .and_then(|thread| thread.iter().position(|c| c.id == comment.id))
        .map_or(1, |index| {
            crate::content::comment_settings::page_of(index, settings.per_page)
        });
    if page > 1 {
        with_query(&item_path, "page", &page.to_string())
    } else {
        item_path
    }
}

/// Render the comment thread and form for an item page.
///
/// Returns an empty string when there is nothing to show: comments disabled,
/// comments hidden for the item's type, or a template that failed to render.
/// An item page must not fail because its comment section did.
///
/// The type's comment settings decide the rest: closed shows the thread and
/// no form; the thread is threaded or flat, oldest or newest first, and paged
/// at the type's count (50 unless it says otherwise) with `page` (1-based; a
/// page past the end is the last page).
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
    requested_page: Option<usize>,
) -> String {
    use crate::content::comment_settings::{CommentMode, page_window};

    let Some(comments) = state.comments_if_enabled() else {
        return String::new();
    };

    let settings = settings_for_type(state, &item.item_type).await;
    if settings.mode == CommentMode::Hidden {
        return String::new();
    }

    let thread = match ordered_thread(comments, item.id, settings).await {
        Ok(thread) => thread,
        Err(e) => {
            tracing::warn!(item_id = %item.id, error = %e, "failed to load comments for item page");
            return String::new();
        }
    };

    let (page, pages, window) = page_window(thread.len(), settings.per_page, requested_page);
    let shown = &thread[window];

    // Author names, one cached lookup per distinct author on this page.
    // An author's name links to their account page when this viewer may read
    // it (B156): an active account, and `access user profiles`.
    let may_read_profiles = viewer.is_admin()
        || viewer.has_permission(super::user_profile::ACCESS_USER_PROFILES)
        || viewer.has_permission("administer users");
    let mut names: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();
    let mut linked: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for comment in shown {
        if !names.contains_key(&comment.author_id)
            && let Ok(Some(user)) = state.users().find_by_id(comment.author_id).await
        {
            if may_read_profiles && user.is_active() && !user.is_anonymous() {
                linked.insert(user.id);
            }
            names.insert(comment.author_id, user.name);
        }
    }

    // Edit and Delete links where the pages behind them would let the viewer
    // in, decided from the permissions already loaded: no query per comment.
    let rendered: Vec<serde_json::Value> = shown
        .iter()
        .map(|comment| {
            serde_json::json!({
                "id": comment.id,
                "depth": if settings.threaded { comment.depth } else { 0 },
                "created": comment.created,
                "author_name": names.get(&comment.author_id),
                // Plain text: the template prints it with autoescape on, never
                // `| safe`, the same rule `body_html` is the one exception to.
                "subject": comment.subject,
                "author_url": linked
                    .contains(&comment.author_id)
                    .then(|| format!("/user/{}", comment.author_id)),
                "body_html": render_comment_body(comment),
                "can_edit": crate::services::comment::permission_allows(comment, "edit", viewer),
                "can_delete": crate::services::comment::permission_allows(comment, "delete", viewer),
            })
        })
        .collect();

    // Posting is what the create route will actually allow, asked the same way.
    let open = settings.mode == CommentMode::Open;
    let can_comment = open
        && viewer.authenticated
        && (viewer.is_admin() || viewer.has_permission("post comments"));

    let mut context = tera::Context::new();
    context.insert("comments", &rendered);
    context.insert("total", &thread.len());
    context.insert("item_id", &item.id);
    context.insert("comments_open", &open);
    context.insert("can_comment", &can_comment);
    context.insert("can_reply", &can_comment);
    context.insert("user_logged_in", &viewer.authenticated);
    context.insert("current_path", &current_path);
    context.insert("comment_outcome", &outcome);
    context.insert("comment_page", &page);
    context.insert("comment_pages", &pages);
    if can_comment {
        // Built through the forms pipeline, only for a viewer who can post.
        // The form posts its own `_token`; the JS layer reads the meta tag in
        // the head instead.
        let form = build_comment_form(item.id, None, CommentFormValues::default(), true, "");
        match super::admin_form::render_as(state, session, viewer, form, &[]).await {
            Ok(html) => context.insert("comment_form", &html),
            Err(_) => tracing::warn!(item_id = %item.id, "failed to build the comment form"),
        }
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
// Comment pages: reply, edit own, delete own
// =============================================================================

/// What a comment page is about: the item, and the address it is read at.
struct PageTarget {
    item: crate::models::Item,
    item_path: String,
}

/// Load an item the viewer may see, or the 404 a missing one gets.
///
/// Denied and missing answer alike, as the item page does, so a comment page
/// cannot be used to learn that an unpublished item exists.
async fn viewable_item(
    state: &AppState,
    viewer: &crate::tap::UserContext,
    item_id: Uuid,
) -> Result<PageTarget, axum::response::Response> {
    use crate::routes::helpers::{render_not_found, render_server_error};
    let item = match state.items().load(item_id).await {
        Ok(Some(item)) => item,
        Ok(None) => return Err(render_not_found()),
        Err(e) => {
            tracing::error!(error = %e, "failed to load item for a comment page");
            return Err(render_server_error("Failed to load the item"));
        }
    };
    if !state
        .items()
        .check_access(&item, "view", viewer)
        .await
        .unwrap_or(false)
    {
        return Err(render_not_found());
    }
    let item_path = item_path_for(state, item.id).await;
    Ok(PageTarget { item, item_path })
}

/// Load a comment the viewer may see on an item they may see.
///
/// A comment that is not published is seen only by its author and a
/// moderator, the rule [`get_comment`] applies.
async fn viewable_comment(
    state: &AppState,
    viewer: &crate::tap::UserContext,
    id: Uuid,
) -> Result<(Comment, PageTarget), axum::response::Response> {
    use crate::routes::helpers::{render_not_found, render_server_error};
    let comment = match state.comments().load(id).await {
        Ok(Some(comment)) => comment,
        Ok(None) => return Err(render_not_found()),
        Err(e) => {
            tracing::error!(error = %e, "failed to load comment for a comment page");
            return Err(render_server_error("Failed to load the comment"));
        }
    };
    let target = viewable_item(state, viewer, comment.item_id).await?;
    if comment.status != CommentStatus::Published.as_i16() {
        let is_author = viewer.authenticated && viewer.id == comment.author_id;
        if !is_author && !viewer.can("administer comments") {
            return Err(render_not_found());
        }
    }
    Ok((comment, target))
}

/// A comment as the comment pages show it: author, date and rendered body.
async fn comment_for_page(state: &AppState, comment: &Comment) -> serde_json::Value {
    let author_name = state
        .users()
        .find_by_id(comment.author_id)
        .await
        .ok()
        .flatten()
        .map(|u| u.name);
    serde_json::json!({
        "id": comment.id,
        "created": comment.created,
        "author_name": author_name,
        "subject": comment.subject,
        "body_html": render_comment_body(comment),
    })
}

/// Render `comment-page.html` inside the site theme.
///
/// One template for the three pages, switched on `mode`, so a theme overrides
/// all three in one place.
async fn render_comment_page(
    state: &AppState,
    session: &Session,
    path: &str,
    status: StatusCode,
    mut context: tera::Context,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    // The page's own heading is its <title> too.
    if let Some(title) = context.get("page_title").cloned() {
        context.insert("title", &title);
    }
    crate::routes::helpers::inject_site_context(state, session, &mut context, path).await;
    let template = state
        .theme()
        .resolve_template(&["comment-page"])
        .unwrap_or_else(|| "comment-page.html".to_string());
    match state.theme().tera().render(&template, &context) {
        Ok(html) => (status, axum::response::Html(html)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render a comment page");
            crate::routes::helpers::render_server_error("Failed to render the page")
        }
    }
}

/// The refusal a signed-in viewer gets for a comment page they may not use.
fn comment_page_forbidden(message: &str) -> axum::response::Response {
    crate::routes::helpers::render_forbidden(message)
}

/// The reply form for an item, or for one comment on it.
///
/// GET /comment/reply/{item_id} and GET /comment/reply/{item_id}/{parent_id}
///
/// A page of its own, as Drupal's `comment/reply/<nid>` is: the item page's
/// Reply link leads here, so replying works without JavaScript and an old
/// link to the reply form still has somewhere to go. The form posts to the
/// comment route, which redirects back to the item.
///
/// A refused submission of the comment form comes back here, showing
/// `values` and `errors` with a `422`. A preview ([`preview_page`]) comes
/// back here too, with `preview`, the comment as it would be posted, shown
/// above the form.
async fn reply_page(
    state: &AppState,
    session: &Session,
    item_id: Uuid,
    parent_id: Option<Uuid>,
    values: CommentFormValues<'_>,
    errors: &[crate::form::ValidationError],
    preview: Option<serde_json::Value>,
) -> axum::response::Response {
    let viewer = crate::routes::item::get_user_context(session, state).await;
    let path = match parent_id {
        Some(parent) => format!("/comment/reply/{item_id}/{parent}"),
        None => format!("/comment/reply/{item_id}"),
    };
    if !viewer.authenticated {
        // Signed out: the login form, which brings the visitor back here.
        return axum::response::IntoResponse::into_response(axum::response::Redirect::to(
            &format!("/user/login?destination={}", urlencoding::encode(&path)),
        ));
    }
    let target = match viewable_item(state, &viewer, item_id).await {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    if !viewer.can("post comments") {
        return comment_page_forbidden("You are not authorized to post comments.");
    }
    if settings_for_type(state, &target.item.item_type).await.mode
        != crate::content::comment_settings::CommentMode::Open
    {
        return comment_page_forbidden("Comments are closed for this item.");
    }
    let parent = match parent_id {
        Some(parent_id) => match viewable_comment(state, &viewer, parent_id).await {
            Ok((parent, _)) if parent.item_id == item_id => Some(parent),
            Ok(_) => return crate::routes::helpers::render_not_found(),
            Err(refusal) => return refusal,
        },
        None => None,
    };

    let mut context = tera::Context::new();
    context.insert("mode", "reply");
    context.insert(
        "page_title",
        if parent.is_some() {
            "Reply to comment"
        } else {
            "Add new comment"
        },
    );
    if let Some(parent) = &parent {
        context.insert("comment", &comment_for_page(state, parent).await);
    }
    if let Some(preview) = &preview {
        context.insert("preview", preview);
    }
    context.insert("item_title", &target.item.title);
    context.insert("item_path", &target.item_path);
    let form = build_comment_form(item_id, parent_id, values, false, &target.item_path);
    match super::admin_form::render_as(state, session, &viewer, form, errors).await {
        Ok(html) => context.insert("comment_form", &html),
        Err(refusal) => return refusal,
    }
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    render_comment_page(state, session, &path, status, context).await
}

async fn reply_to_item(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<Uuid>,
) -> axum::response::Response {
    reply_page(
        &state,
        &session,
        item_id,
        None,
        CommentFormValues::default(),
        &[],
        None,
    )
    .await
}

async fn reply_to_comment(
    State(state): State<AppState>,
    session: Session,
    Path((item_id, parent_id)): Path<(Uuid, Uuid)>,
) -> axum::response::Response {
    reply_page(
        &state,
        &session,
        item_id,
        Some(parent_id),
        CommentFormValues::default(),
        &[],
        None,
    )
    .await
}

/// The signed-in viewer and a comment they may perform `operation` on, or the
/// page that refuses them.
async fn comment_for_operation(
    state: &AppState,
    session: &Session,
    id: Uuid,
    operation: &str,
) -> Result<(crate::tap::UserContext, Comment, PageTarget), axum::response::Response> {
    let user = crate::routes::helpers::require_login(state, session).await?;
    let viewer = user_context_for(state, &user).await;
    let (comment, target) = viewable_comment(state, &viewer, id).await?;
    let allowed = state
        .comments()
        .check_access(&comment, operation, &viewer)
        .await
        .unwrap_or(false);
    if !allowed {
        return Err(comment_page_forbidden(match operation {
            "edit" => "You are not authorized to edit this comment.",
            _ => "You are not authorized to delete this comment.",
        }));
    }
    Ok((viewer, comment, target))
}

/// The edit form for a comment, `comment_edit`, showing `body` and `subject`.
fn build_edit_form(
    comment: &Comment,
    target: &PageTarget,
    body: &str,
    subject: &str,
) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    Form::new("comment_edit")
        .action(format!("/comment/{}/edit", comment.id))
        .attribute("class", "comment-form")
        .element(
            "subject",
            FormElement::textfield()
                .title("Subject (optional)")
                .max_length(crate::models::comment::SUBJECT_MAX_CHARS)
                .default_value(subject)
                .weight(0),
        )
        .element(
            "body",
            FormElement::textarea(8)
                .title("Comment")
                .required()
                .default_value(body)
                .weight(1),
        )
        .element(
            "op",
            FormElement::submit("Save")
                .suffix(cancel_link(&format!(
                    "{}#comment-{}",
                    target.item_path, comment.id
                )))
                .weight(100),
        )
}

/// The delete confirmation form for a comment, `comment_delete`.
fn build_delete_form(comment: &Comment, target: &PageTarget) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    Form::new("comment_delete")
        .action(format!("/comment/{}/delete", comment.id))
        .attribute("class", "comment-delete-form")
        .element(
            "warning",
            FormElement::markup(
                "<p>Are you sure you want to delete this comment? Any replies to it will be \
                 deleted too. This action cannot be undone.</p>",
            )
            .weight(0),
        )
        .element(
            "op",
            FormElement::submit("Delete")
                .suffix(cancel_link(&format!(
                    "{}#comment-{}",
                    target.item_path, comment.id
                )))
                .weight(100),
        )
}

/// Render the edit page for `comment`, its form showing `body`, `subject`
/// and `errors`.
#[allow(clippy::too_many_arguments)]
async fn render_edit_page(
    state: &AppState,
    session: &Session,
    viewer: &crate::tap::UserContext,
    comment: &Comment,
    target: &PageTarget,
    body: &str,
    subject: &str,
    errors: &[crate::form::ValidationError],
) -> axum::response::Response {
    let mut context = tera::Context::new();
    context.insert("mode", "edit");
    context.insert("page_title", "Edit comment");
    context.insert("comment", &comment_for_page(state, comment).await);
    context.insert("item_title", &target.item.title);
    context.insert("item_path", &target.item_path);
    let form = build_edit_form(comment, target, body, subject);
    match super::admin_form::render_as(state, session, viewer, form, errors).await {
        Ok(html) => context.insert("comment_form", &html),
        Err(refusal) => return refusal,
    }
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    render_comment_page(
        state,
        session,
        &format!("/comment/{}/edit", comment.id),
        status,
        context,
    )
    .await
}

/// The edit form for a comment.
///
/// GET /comment/{id}/edit
///
/// For the comment's author with `edit own comments`, and for anyone with
/// `administer comments`. The moderation screen's edit form, with its status
/// control, stays the administrator's; this one edits the text only.
async fn edit_page(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    let (viewer, comment, target) = match comment_for_operation(&state, &session, id, "edit").await
    {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    render_edit_page(
        &state,
        &session,
        &viewer,
        &comment,
        &target,
        &comment.body,
        comment.subject.as_deref().unwrap_or(""),
        &[],
    )
    .await
}

/// Save an edited comment.
///
/// POST /comment/{id}/edit
async fn edit_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    axum::Form(pairs): axum::Form<Vec<(String, String)>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let posted = crate::content::submission::PostedForm(pairs);
    let (viewer, comment, target) = match comment_for_operation(&state, &session, id, "edit").await
    {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let body = posted.get("body").unwrap_or_default().to_string();
    // The form always carries the field's full, current value rather than a
    // partial change, so an empty subject clears it.
    let subject_field = posted.get("subject").unwrap_or_default().to_string();
    let mut errors = Vec::new();
    if body.trim().is_empty() {
        errors.push(crate::form::ValidationError::form(
            "Comment body cannot be empty.",
        ));
    }
    let subject = validate_subject(Some(&subject_field));
    if subject.is_err() {
        errors.push(crate::form::ValidationError::form(format!(
            "Subject cannot exceed {SUBJECT_MAX_CHARS} characters."
        )));
    }
    let form_state = match super::admin_form::check_as(
        &state,
        &session,
        &viewer,
        &posted,
        "comment_edit",
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Err(refusal)) => return refusal,
        Err(Ok(errors)) => {
            return render_edit_page(
                &state,
                &session,
                &viewer,
                &comment,
                &target,
                &body,
                &subject_field,
                &errors,
            )
            .await;
        }
    };
    let Ok(subject) = subject else {
        return crate::routes::helpers::render_server_error("Failed to update the comment");
    };
    let input = UpdateComment {
        body: Some(body),
        body_format: None,
        status: None,
        subject: Some(subject),
    };
    match state.comments().update(id, input, &viewer).await {
        Ok(Some(updated)) => {
            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    None,
                    &super::item_form::tap_state(&state, &viewer),
                )
                .await;
            axum::response::Redirect::to(&format!(
                "{}#comment-{id}",
                with_query(
                    &comment_location(&state, &updated).await,
                    "comment",
                    "updated"
                )
            ))
            .into_response()
        }
        Ok(None) => crate::routes::helpers::render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update comment");
            crate::routes::helpers::render_server_error("Failed to update the comment")
        }
    }
}

/// The confirmation page before a comment is deleted.
///
/// GET /comment/{id}/delete
///
/// For the comment's author with `delete own comments`, and for anyone with
/// `administer comments`. Deleting takes the comment's replies with it, as
/// Drupal's does, and the page says so.
async fn delete_page(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    let (viewer, comment, target) =
        match comment_for_operation(&state, &session, id, "delete").await {
            Ok(found) => found,
            Err(refusal) => return refusal,
        };
    render_delete_page(&state, &session, &viewer, &comment, &target, &[]).await
}

/// Render the delete confirmation for `comment`, its form showing `errors`.
async fn render_delete_page(
    state: &AppState,
    session: &Session,
    viewer: &crate::tap::UserContext,
    comment: &Comment,
    target: &PageTarget,
    errors: &[crate::form::ValidationError],
) -> axum::response::Response {
    let mut context = tera::Context::new();
    context.insert("mode", "delete");
    context.insert("page_title", "Delete comment");
    context.insert("comment", &comment_for_page(state, comment).await);
    context.insert("item_title", &target.item.title);
    context.insert("item_path", &target.item_path);
    let form = build_delete_form(comment, target);
    match super::admin_form::render_as(state, session, viewer, form, errors).await {
        Ok(html) => context.insert("comment_form", &html),
        Err(refusal) => return refusal,
    }
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    render_comment_page(
        state,
        session,
        &format!("/comment/{}/delete", comment.id),
        status,
        context,
    )
    .await
}

/// Delete a comment, once confirmed.
///
/// POST /comment/{id}/delete
async fn delete_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    axum::Form(pairs): axum::Form<Vec<(String, String)>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let posted = crate::content::submission::PostedForm(pairs);
    let (viewer, comment, target) =
        match comment_for_operation(&state, &session, id, "delete").await {
            Ok(found) => found,
            Err(refusal) => return refusal,
        };
    let form_state = match super::admin_form::check_as(
        &state,
        &session,
        &viewer,
        &posted,
        "comment_delete",
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Err(refusal)) => return refusal,
        Err(Ok(errors)) => {
            return render_delete_page(&state, &session, &viewer, &comment, &target, &errors).await;
        }
    };
    match state.comments().delete(id, &viewer).await {
        Ok(true) => {
            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    None,
                    &super::item_form::tap_state(&state, &viewer),
                )
                .await;
            axum::response::Redirect::to(&format!("{}?comment=deleted#comments", target.item_path))
                .into_response()
        }
        Ok(false) => crate::routes::helpers::render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete comment");
            crate::routes::helpers::render_server_error("Failed to delete the comment")
        }
    }
}

/// The query a comment permalink passes on: what just happened to it.
#[derive(Debug, Deserialize)]
pub struct PermalinkQuery {
    /// `posted`, `pending` or `updated`; anything else is dropped.
    pub comment: Option<String>,
}

/// A comment's own address.
///
/// GET /comment/{id}
///
/// Redirects to the item, on the page of the thread that holds the comment,
/// at the comment. comment-post.js goes here after a comment is posted, with
/// `?comment=posted`, so the writer sees their comment and the confirmation
/// whichever page of a long thread it landed on. An id that is not one, a
/// comment that does not exist and one the viewer may not see are all 404.
async fn permalink(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Query(query): Query<PermalinkQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Ok(id) = id.parse::<Uuid>() else {
        return crate::routes::helpers::render_not_found();
    };
    let viewer = crate::routes::item::get_user_context(&session, &state).await;
    let comment = match viewable_comment(&state, &viewer, id).await {
        Ok((comment, _)) => comment,
        Err(refusal) => return refusal,
    };
    let mut target = comment_location(&state, &comment).await;
    if let Some(outcome) = query
        .comment
        .as_deref()
        .filter(|o| matches!(*o, "posted" | "pending" | "updated"))
    {
        target = with_query(&target, "comment", outcome);
    }
    axum::response::Redirect::to(&format!("{target}#comment-{id}")).into_response()
}

/// The comment pages, served while `trovato_comments` is enabled.
pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route("/comment/{id}", get(permalink))
        .route("/comment/reply/{item_id}", get(reply_to_item))
        .route(
            "/comment/reply/{item_id}/{parent_id}",
            get(reply_to_comment),
        )
        .route("/comment/{id}/edit", get(edit_page).post(edit_submit))
        .route("/comment/{id}/delete", get(delete_page).post(delete_submit))
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

/// What a comment notification needs besides the comment: loaded before the
/// write, so the message can be queued inside the write's transaction (BG18).
pub(crate) struct CommentMail {
    to: String,
    site_name: String,
    action_url: String,
    item_title: String,
    item_author_id: uuid::Uuid,
    commenter_name: String,
}

/// Load what [`comment_notification`] needs to mail `item`'s author, or `None`
/// when there is no mail to send: mail is not configured, the site has turned
/// the notice off ([`crate::models::comment::notify_authors`]), or the author
/// is gone or has no address.
///
/// Both notice paths, a comment posted visible here and a held one approved
/// in `routes::admin`, come through this, so the setting is read in one place.
pub(crate) async fn comment_mail_context(
    state: &AppState,
    item: &crate::models::Item,
    commenter_name: Option<&str>,
) -> Option<CommentMail> {
    let email_service = state.email()?;
    if !crate::models::comment::notify_authors(state.db()).await {
        return None;
    }
    let author = match state.users().find_by_id(item.author_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            tracing::debug!("comment notification: author not found");
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, "comment notification: failed to load author");
            return None;
        }
    };
    if author.mail.is_empty() {
        return None;
    }
    let site_name = crate::models::SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    Some(CommentMail {
        to: author.mail,
        site_name,
        action_url: format!("{}/item/{}", email_service.site_url(), item.id),
        item_title: item.title.clone(),
        item_author_id: item.author_id,
        commenter_name: commenter_name.unwrap_or("Someone").to_string(),
    })
}

/// The notification mailing the content author that `comment` has just become
/// visible, or `None` when [`should_notify_on_publish`] says it has not.
/// `previous` is the status before the change, `None` for a new comment.
///
/// One message per comment: the dedupe key makes a second approval, or a
/// retried request, queue nothing.
pub(crate) fn comment_notification(
    tera: &tera::Tera,
    ctx: &CommentMail,
    comment: &Comment,
    previous: Option<i16>,
) -> Option<crate::services::email::NewMail> {
    if !should_notify_on_publish(
        previous,
        comment.status,
        comment.author_id,
        ctx.item_author_id,
    ) {
        return None;
    }
    // The comment's own subject when it has one; otherwise the mail's own
    // subject text, as it has always read.
    let subject = comment
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("New comment on \"{}\" at {}", ctx.item_title, ctx.site_name));

    // Truncate comment preview for email. Walks back to a char boundary: a
    // multi-byte character straddling byte 500 would panic a bare slice.
    let comment_text = comment.body.as_str();
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
    context.insert("site_name", &ctx.site_name);
    context.insert("commenter_name", &ctx.commenter_name);
    context.insert("content_title", &ctx.item_title);
    context.insert("comment_text", preview);
    context.insert("action_url", &ctx.action_url);
    context.insert("subject", &subject);

    match crate::services::email_templates::render(tera, "comment_notification", &context) {
        Ok((html, text)) => {
            let message = crate::services::email::NewMail::notification(
                "comment_notification",
                &ctx.to,
                &subject,
                &text,
            )
            .html(html)
            .dedupe(format!("comment-published:{}", comment.id));
            // A notification that cannot be sent (an author address the mail
            // library will not parse) must not cost the commenter their comment.
            match message.validate() {
                Ok(()) => Some(message),
                Err(e) => {
                    tracing::warn!(error = %e, comment_id = %comment.id, "comment notification cannot be sent; comment kept");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "comment notification: failed to render template");
            None
        }
    }
}

// =============================================================================
// Router
// =============================================================================

/// The comment JSON API, under `/api/v1` ([`super::api`]), served while
/// `trovato_comments` is enabled.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("comments")
        .get(
            "/item/{id}/comments",
            list_item_comments,
            doc("List an item's comments in thread order")
                .query("include", "`author` to embed each comment's author")
                .paged(50, 100)
                .alias(),
        )
        .post(
            "/item/{id}/comments",
            create_comment,
            doc("Post a comment on an item (JSON or form body; CSRF token)").alias(),
        )
        .get("/comment/{id}", get_comment, doc("Get a comment").alias())
        .put(
            "/comment/{id}",
            update_comment,
            doc("Edit a comment").alias(),
        )
        .delete(
            "/comment/{id}",
            delete_comment,
            doc("Delete a comment").alias(),
        )
        .plugin("trovato_comments")
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
        CommentSubmission::from_form(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
    }

    /// A form submission carries its CSRF token in the body, because an HTML form
    /// cannot set a header. That is why posting the rendered form used to fail.
    #[test]
    fn a_form_submission_takes_its_csrf_token_from_the_body() {
        let submission = form(&[("body", "Hello"), ("_token", "token-value")]);

        assert_eq!(submission.body, "Hello");
        assert_eq!(
            submission.posted.as_ref().and_then(|p| p.get("_token")),
            Some("token-value")
        );
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

    /// A missing `_token` still routes to the form check rather than falling
    /// through to the header one, so a form post cannot skip CSRF by omitting the
    /// field.
    #[test]
    fn a_form_submission_without_a_token_still_checks_the_body() {
        let submission = form(&[("body", "Hello")]);

        assert!(submission.posted.is_some());
        assert_eq!(
            submission.posted.as_ref().and_then(|p| p.get("_token")),
            None
        );
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
