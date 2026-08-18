//! Core admin routes: dashboard, stage management, file management,
//! comment moderation, and AJAX callbacks.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::file::service::FileStatus;
use crate::form::AjaxRequest;
use crate::models::{Comment, CommentStatus, SiteConfig, UpdateComment};
use crate::routes::auth::SESSION_ACTIVE_STAGE;
use crate::state::AppState;

use crate::form::csrf::generate_csrf_token;

use crate::error::AppError;

use super::helpers::{
    CsrfOnlyForm, admin_user_context, render_admin_template, render_not_found, render_server_error,
    require_admin, require_admin_json, require_csrf,
};

/// Stage switch request.
#[derive(Debug, Deserialize)]
struct StageSwitchRequest {
    /// Stage ID to switch to. None means "live" (production).
    stage_id: Option<String>,
}

/// Stage switch response.
#[derive(Debug, Serialize)]
struct StageSwitchResponse {
    success: bool,
    active_stage: Option<String>,
}

/// Switch the active stage for the current session.
///
/// POST /admin/stage/switch
async fn switch_stage(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(request): Json<StageSwitchRequest>,
) -> Result<Json<StageSwitchResponse>, AppError> {
    // Verify CSRF token from header
    super::helpers::require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;

    require_admin_json(&state, &session).await?;

    session
        .insert(SESSION_ACTIVE_STAGE, request.stage_id.clone())
        .await
        .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "switch stage"))?;

    tracing::info!(stage = ?request.stage_id, "stage switched");

    Ok(Json(StageSwitchResponse {
        success: true,
        active_stage: request.stage_id,
    }))
}

/// Get the current active stage.
async fn get_current_stage(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<StageSwitchResponse>, AppError> {
    require_admin_json(&state, &session).await?;

    let active_stage: Option<String> = session
        .get(SESSION_ACTIVE_STAGE)
        .await
        .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "get current stage"))?
        .flatten();

    Ok(Json(StageSwitchResponse {
        success: true,
        active_stage,
    }))
}

// =============================================================================
// Admin Dashboard
// =============================================================================

/// Admin dashboard.
///
/// GET /admin
async fn dashboard(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let content_types = state.content_types().list_all().await;

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin");
    context.insert("user", &user);
    context.insert("csrf_token", &csrf_token);

    // The update banner, read from what the last cron check stored. Only here, and
    // only past `require_admin`: a visitor has no use for the site's version and no
    // business being told it. Nothing is fetched on this path — a page render never
    // makes an outbound request.
    if let Some(status) = crate::update_status::stored_status(state.db()).await
        && status.is_behind()
    {
        context.insert("update_status", &status);
    }
    context.insert("running_version", crate::update_status::running_version());

    render_admin_template(&state, "admin/dashboard.html", context).await
}

// =============================================================================
// File Management
// =============================================================================

/// List all files.
///
/// GET /admin/content/files
async fn list_files(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let status_filter = params.get("status").and_then(|s| match s.as_str() {
        "0" => Some(FileStatus::Temporary),
        "1" => Some(FileStatus::Permanent),
        _ => None,
    });

    let files = match state.files().list_by_status(status_filter, 100, 0).await {
        Ok(files) => files,
        Err(e) => {
            tracing::error!(error = %e, "failed to list files");
            return render_server_error("Failed to load files.");
        }
    };

    // Get owners for display
    let mut owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for file in &files {
        if !owners.contains_key(&file.owner_id.to_string())
            && let Ok(Some(user)) = state.users().find_by_id(file.owner_id).await
        {
            owners.insert(file.owner_id.to_string(), user.name);
        }
    }

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("files", &files);
    context.insert("owners", &owners);
    context.insert("status_filter", &status_filter.map(|s| s as i16));
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/content/files");

    render_admin_template(&state, "admin/files.html", context).await
}

/// Show file details.
///
/// GET /admin/content/files/{id}
async fn file_details(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(file) = state.files().get(file_id).await.ok().flatten() else {
        return render_not_found();
    };

    let owner = state.users().find_by_id(file.owner_id).await.ok().flatten();
    let public_url = state.files().storage().public_url(&file.uri);
    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("file", &file);
    context.insert("owner", &owner);
    context.insert("public_url", &public_url);
    context.insert("path", &format!("/admin/content/files/{file_id}"));

    render_admin_template(&state, "admin/file-details.html", context).await
}

/// Delete a file.
///
/// POST /admin/content/files/{id}/delete
async fn delete_file(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state.files().delete(file_id).await {
        Ok(true) => {
            tracing::info!(file_id = %file_id, "file deleted");
            Redirect::to("/admin/content/files").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete file");
            render_server_error("Failed to delete file.")
        }
    }
}

// =============================================================================
// Media Library
// =============================================================================

/// Form data for editing a file's alternative text.
#[derive(Debug, Deserialize)]
struct AltTextForm {
    #[serde(rename = "_token")]
    token: String,
    /// The alt text. Empty means "explicitly decorative", which is a real answer
    /// rather than an absent one.
    alt_text: String,
    /// Where to return to, so the media library and the file details page can
    /// share one endpoint without one of them throwing the user to the other.
    #[serde(default)]
    redirect_to: Option<String>,
}

/// Set a file's alternative text.
///
/// POST /admin/content/files/{id}/alt-text
async fn set_file_alt_text(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
    Form(form): Form<AltTextForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state
        .files()
        .set_alt_text(file_id, Some(&form.alt_text))
        .await
    {
        Ok(true) => {
            // Only a same-site path is honoured, so the return target cannot be
            // turned into an open redirect by a crafted form post.
            let target = form
                .redirect_to
                .filter(|path| path.starts_with('/') && !path.starts_with("//"))
                .unwrap_or_else(|| format!("/admin/content/files/{file_id}"));
            Redirect::to(&target).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to set file alt text");
            render_server_error("Failed to save alternative text.")
        }
    }
}

/// Media library page with grid display.
///
/// GET /admin/media
async fn media_library(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let page_size: i64 = 24;
    let offset = (page - 1) * page_size;

    let type_filter = params.get("type").map(String::as_str);
    let search = params.get("q").map(String::as_str);

    let mime_prefix: Option<&str> = match type_filter {
        Some("image") => Some("image/"),
        Some("document") => Some("application/"),
        _ => None,
    };

    let files = match state
        .files()
        .list_filtered_media(
            Some(FileStatus::Permanent),
            mime_prefix,
            search,
            "newest",
            page_size,
            offset,
        )
        .await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "failed to list media");
            return render_server_error("Failed to load media library.");
        }
    };

    let total = state
        .files()
        .count_filtered_media(Some(FileStatus::Permanent), mime_prefix, search)
        .await
        .unwrap_or(0);

    // Build public URLs for each file
    let storage = state.files().storage().clone();
    let file_urls: std::collections::HashMap<String, String> = files
        .iter()
        .map(|f| (f.id.to_string(), storage.public_url(&f.uri)))
        .collect();

    let total_pages = (total + page_size - 1) / page_size;

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("files", &files);
    context.insert("file_urls", &file_urls);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("page_size", &page_size);
    context.insert("total_pages", &total_pages);
    context.insert("type_filter", &type_filter.unwrap_or("all"));
    context.insert("search", &search.unwrap_or(""));
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/media");

    render_admin_template(&state, "admin/media-library.html", context).await
}

// =============================================================================
// AJAX Endpoint
// =============================================================================

/// AJAX form callback endpoint.
///
/// POST /system/ajax
async fn ajax_callback(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(request): Json<AjaxRequest>,
) -> Response {
    use crate::form::AjaxResponse;
    use crate::tap::RequestState;

    // Verify CSRF token from header
    if let Err((status, json)) = super::helpers::require_csrf_header(&session, &headers).await {
        return (status, json).into_response();
    }

    // Require authentication for AJAX requests
    let Ok(user) = require_admin(&state, &session).await else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(AjaxResponse::new().alert("Session expired. Please log in again.")),
        )
            .into_response();
    };

    // Handle admin-specific AJAX triggers
    if request.trigger == "add_field" {
        return super::admin_content_type::handle_ajax_add_field(&state, &request).await;
    }

    // Build the acting context through the shared loader so the AJAX callback
    // sees the admin's real permissions. This used to hard-code
    // `vec!["administer site"]`, dropping them exactly as the old front page did.
    let user_context = admin_user_context(&state, &user).await;
    let request_state = RequestState::without_services(user_context);

    match state
        .forms()
        .ajax_callback(&request, &session, &request_state)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "AJAX callback failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AjaxResponse::new().alert("An error occurred. Please try again.")),
            )
                .into_response()
        }
    }
}

// =============================================================================
// Comment Moderation
// =============================================================================

/// Query params for comment list.
#[derive(Debug, Deserialize)]
struct CommentListQuery {
    status: Option<i16>,
    page: Option<i64>,
}

/// Form data for editing a comment.
#[derive(Debug, Deserialize)]
struct EditCommentForm {
    #[serde(rename = "_token")]
    token: String,
    body: String,
    status: i16,
}

/// Form data for the moderation default setting.
#[derive(Debug, Deserialize)]
struct CommentSettingsForm {
    #[serde(rename = "_token")]
    token: String,
    /// `published` or `pending`.
    default_status: String,
}

/// A comment as the moderation list renders it.
///
/// The list used to decide the label in the template with
/// `{% if comment.status == 1 %}`, which is why an unpublished comment displayed
/// as "Pending" — one branch for four states. The status name now comes from
/// [`CommentStatus`], so the screen and the model cannot disagree.
#[derive(Debug, Serialize)]
struct CommentRow<'a> {
    #[serde(flatten)]
    comment: &'a Comment,
    /// Human-readable status.
    status_label: &'static str,
    /// CSS class suffix for the status badge.
    status_class: &'static str,
    /// Whether the approve action applies.
    can_approve: bool,
    /// Whether the unpublish action applies.
    can_unpublish: bool,
    /// Whether the mark-as-spam action applies.
    can_mark_spam: bool,
}

impl<'a> CommentRow<'a> {
    fn new(comment: &'a Comment) -> Self {
        // An unrecognised stored value is shown as unpublished, matching how the
        // read paths treat it: not visible.
        let status = CommentStatus::from_i16(comment.status).unwrap_or(CommentStatus::Unpublished);
        Self {
            comment,
            status_label: status.label(),
            status_class: status.css_suffix(),
            can_approve: !status.is_visible(),
            can_unpublish: status.is_visible(),
            can_mark_spam: status != CommentStatus::Spam,
        }
    }
}

/// The status options both comment screens render, as `{value, label}` pairs.
fn comment_status_options() -> Vec<serde_json::Value> {
    COMMENT_STATUS_FILTERS
        .iter()
        .map(|s| {
            serde_json::json!({
                "value": s.as_i16().to_string(),
                "label": s.label(),
            })
        })
        .collect()
}

/// The statuses the moderation screens offer, in display order.
const COMMENT_STATUS_FILTERS: [CommentStatus; 4] = [
    CommentStatus::Pending,
    CommentStatus::Published,
    CommentStatus::Unpublished,
    CommentStatus::Spam,
];

/// List all comments for moderation.
///
/// GET /admin/content/comments
async fn list_comments(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<CommentListQuery>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let page = query.page.unwrap_or(1).max(1);
    let per_page: i64 = 25;
    let offset = (page - 1) * per_page;

    let comments = if let Some(status) = query.status {
        state
            .comments()
            .list_by_status(status, per_page, offset)
            .await
            .unwrap_or_default()
    } else {
        state
            .comments()
            .list_all(per_page, offset)
            .await
            .unwrap_or_default()
    };

    let total = state.comments().count_all().await.unwrap_or(0);

    // Get author names
    let mut authors: std::collections::HashMap<uuid::Uuid, String> =
        std::collections::HashMap::new();
    for comment in &comments {
        if !authors.contains_key(&comment.author_id)
            && let Ok(Some(user)) = state.users().find_by_id(comment.author_id).await
        {
            authors.insert(comment.author_id, user.name);
        }
    }

    // Get item titles
    let mut items: std::collections::HashMap<uuid::Uuid, String> = std::collections::HashMap::new();
    for comment in &comments {
        if !items.contains_key(&comment.item_id)
            && let Ok(Some(item)) = state.items().load(comment.item_id).await
        {
            items.insert(comment.item_id, item.title);
        }
    }

    let csrf_token = generate_csrf_token(&session).await;

    let rows: Vec<CommentRow<'_>> = comments.iter().map(CommentRow::new).collect();

    // The status filter options, and the current default for new comments, so
    // the screen that moderates comments is also where the queue is turned on.
    let filters = comment_status_options();
    let default_status = CommentStatus::default_for_new_comments(state.db()).await;

    let mut context = tera::Context::new();
    context.insert("comments", &rows);
    context.insert("status_options", &filters);
    context.insert("default_status_pending", &default_status.awaits_review());
    context.insert("authors", &authors);
    context.insert("items", &items);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("per_page", &per_page);
    context.insert(
        "status_filter",
        &query.status.map(|s| s.to_string()).unwrap_or_default(),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/content/comments");

    render_admin_template(&state, "admin/comments.html", context).await
}

/// Edit a comment form.
///
/// GET /admin/content/comments/{id}/edit
async fn edit_comment_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let comment = match state.comments().load(id).await {
        Ok(Some(c)) => c,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load comment");
            return render_server_error("Failed to load comment");
        }
    };

    let author_name = state
        .users()
        .find_by_id(comment.author_id)
        .await
        .ok()
        .flatten()
        .map(|u| u.name);

    let item_title = state
        .items()
        .load(comment.item_id)
        .await
        .ok()
        .flatten()
        .map(|i| i.title);

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("comment", &comment);
    context.insert("status_options", &comment_status_options());
    context.insert("author_name", &author_name);
    context.insert("item_title", &item_title);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/content/comments");

    render_admin_template(&state, "admin/comment-form.html", context).await
}

/// Edit a comment submit.
///
/// POST /admin/content/comments/{id}/edit
async fn edit_comment_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
    Form(form): Form<EditCommentForm>,
) -> Response {
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // A status the model does not recognise is a malformed submission, not a new
    // state to store.
    if CommentStatus::from_i16(form.status).is_none() {
        return render_server_error("Unknown comment status");
    }

    let user_ctx = admin_user_context(&state, &user).await;
    let input = UpdateComment {
        body: Some(form.body),
        body_format: None,
        status: Some(form.status),
    };

    // The status before the edit, so approving out of the queue notifies the
    // content author and re-saving an already published comment does not.
    let previous = state
        .comments()
        .load(id)
        .await
        .ok()
        .flatten()
        .map(|c| c.status);

    match state.comments().update(id, input, &user_ctx).await {
        Ok(Some(comment)) => {
            notify_author_if_published(&state, &comment, previous).await;
            Redirect::to("/admin/content/comments").into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update comment");
            render_server_error("Failed to update comment")
        }
    }
}

/// Mail the content author when a moderation action has just made a comment
/// visible.
///
/// The notification used to fire when a comment was created, which under a
/// hold-for-review default would have mailed the author the full text of every
/// comment the queue exists to catch. It belongs on the transition into
/// published, which is here and in the create route.
async fn notify_author_if_published(state: &AppState, comment: &Comment, previous: Option<i16>) {
    // One rule, in one place: `notify_if_published` decides. This loads the item
    // unconditionally rather than pre-filtering on a placeholder author id,
    // which would wrongly suppress the mail whenever the comment author happened
    // to equal the placeholder. A moderation action can afford one query.
    let Ok(Some(item)) = state.items().load(comment.item_id).await else {
        return;
    };
    let commenter = state
        .users()
        .find_by_id(comment.author_id)
        .await
        .ok()
        .flatten()
        .map(|u| u.name);

    crate::routes::comment::notify_if_published(
        state,
        comment,
        previous,
        &item,
        commenter.as_deref(),
    );
}

/// Set a comment's publication status (shared by approve/unpublish).
async fn set_comment_status(
    state: &AppState,
    session: &Session,
    id: uuid::Uuid,
    token: &str,
    status: i16,
    action: &str,
) -> Response {
    let user = match require_admin(state, session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(session, token).await {
        return resp;
    }

    let user_ctx = admin_user_context(state, &user).await;
    let input = UpdateComment {
        body: None,
        body_format: None,
        status: Some(status),
    };

    let previous = state
        .comments()
        .load(id)
        .await
        .ok()
        .flatten()
        .map(|c| c.status);

    match state.comments().update(id, input, &user_ctx).await {
        Ok(Some(comment)) => {
            notify_author_if_published(state, &comment, previous).await;
            Redirect::to("/admin/content/comments").into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to {} comment", action);
            render_server_error(&format!("Failed to {action} comment"))
        }
    }
}

/// Approve a comment.
///
/// POST /admin/content/comments/{id}/approve
async fn approve_comment(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    set_comment_status(
        &state,
        &session,
        id,
        &form.token,
        CommentStatus::Published.as_i16(),
        "approve",
    )
    .await
}

/// Unpublish a comment.
///
/// POST /admin/content/comments/{id}/unpublish
async fn unpublish_comment(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    set_comment_status(
        &state,
        &session,
        id,
        &form.token,
        CommentStatus::Unpublished.as_i16(),
        "unpublish",
    )
    .await
}

/// Mark a comment as spam.
///
/// Kept rather than deleted, so a false positive can be recovered and a
/// classifier has something to learn from.
///
/// POST /admin/content/comments/{id}/spam
async fn mark_comment_spam(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    set_comment_status(
        &state,
        &session,
        id,
        &form.token,
        CommentStatus::Spam.as_i16(),
        "mark as spam",
    )
    .await
}

/// Set whether new comments are published immediately or held for review.
///
/// The setting lives on the moderation screen because that is where its
/// consequences are: turning the queue on fills the list below it.
///
/// POST /admin/content/comments/settings
async fn save_comment_settings(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CommentSettingsForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Only the two statuses a new comment can meaningfully take.
    let value = match form.default_status.as_str() {
        "published" => "published",
        "pending" => "pending",
        other => {
            tracing::warn!(value = %other, "rejected unknown comment default status");
            return render_server_error("Unknown comment status");
        }
    };

    match SiteConfig::set(
        state.db(),
        crate::models::comment::DEFAULT_STATUS_KEY,
        serde_json::Value::String(value.to_string()),
    )
    .await
    {
        Ok(_) => Redirect::to("/admin/content/comments").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to save comment default status");
            render_server_error("Failed to save setting")
        }
    }
}

/// Delete a comment.
///
/// POST /admin/content/comments/{id}/delete
async fn delete_comment_admin(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let user_ctx = admin_user_context(&state, &user).await;
    match state.comments().delete(id, &user_ctx).await {
        Ok(true) => Redirect::to("/admin/content/comments").into_response(),
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete comment");
            render_server_error("Failed to delete comment")
        }
    }
}

/// Create the admin router.
/// Core admin routes (always registered).
pub fn router() -> Router<AppState> {
    Router::new()
        // Dashboard
        .route("/admin", get(dashboard))
        // Stage management
        .route("/admin/stage/switch", post(switch_stage))
        .route("/admin/stage/current", get(get_current_stage))
        // User, role, and permission management
        .merge(super::admin_user::router())
        // Content management
        .merge(super::admin_content::router())
        // File management
        .route("/admin/content/files", get(list_files))
        .route("/admin/content/files/{id}", get(file_details))
        .route("/admin/content/files/{id}/delete", post(delete_file))
        .route(
            "/admin/content/files/{id}/alt-text",
            post(set_file_alt_text),
        )
        // Media library
        .route("/admin/media", get(media_library))
        // Content type and search configuration management
        .merge(super::admin_content_type::router())
        .merge(super::admin_record_type::router())
        // URL Alias management
        .merge(super::admin_alias::router())
        // Menu link management
        .merge(super::admin_menu::router())
        // Editorial stage management
        .merge(super::admin_stage::router())
        // Pathauto configuration
        .merge(super::admin_pathauto::router())
        // AI Provider management
        .merge(super::admin_ai_provider::router())
        // AI Budget management
        .merge(super::admin_ai_budget::router())
        // AI Chat configuration
        .merge(super::admin_ai_chat::router())
        // Site configuration
        .merge(super::admin_config::router())
        // Plugin-queue dead-letter admin (P11d / D-46)
        .merge(super::admin_queue::router())
        // Async auto-embed lifecycle admin (P11f / D-51, D-52)
        .merge(super::admin_embed::router())
        // AJAX endpoint
        .route("/system/ajax", post(ajax_callback))
}

/// Category and tag admin routes (registered when "categories" plugin is enabled).
pub fn category_admin_router() -> Router<AppState> {
    super::admin_taxonomy::router()
}

/// Comment moderation admin routes (registered when "comments" plugin is enabled).
pub fn comment_admin_router() -> Router<AppState> {
    Router::new()
        .route("/admin/content/comments", get(list_comments))
        .route("/admin/content/comments/{id}/edit", get(edit_comment_form))
        .route(
            "/admin/content/comments/{id}/edit",
            post(edit_comment_submit),
        )
        .route(
            "/admin/content/comments/{id}/approve",
            post(approve_comment),
        )
        .route(
            "/admin/content/comments/{id}/unpublish",
            post(unpublish_comment),
        )
        .route("/admin/content/comments/{id}/spam", post(mark_comment_spam))
        .route(
            "/admin/content/comments/settings",
            post(save_comment_settings),
        )
        .route(
            "/admin/content/comments/{id}/delete",
            post(delete_comment_admin),
        )
}
