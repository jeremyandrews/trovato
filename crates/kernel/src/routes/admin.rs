//! Core admin routes: dashboard, stage management, file management,
//! comment moderation, and AJAX callbacks.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::file::service::FileStatus;
use crate::form::AjaxRequest;
use crate::models::{Comment, Item, UpdateComment, User};
use crate::routes::auth::{SESSION_ACTIVE_STAGE, SESSION_USER_ID};
use crate::state::AppState;

use crate::form::csrf::generate_csrf_token;

use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_admin,
    require_csrf,
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

/// Error response.
#[derive(Debug, Serialize)]
struct AdminError {
    error: String,
}

/// Require admin for JSON API endpoints, returning a JSON error on failure.
async fn require_admin_json(
    state: &AppState,
    session: &Session,
) -> Result<(), (StatusCode, Json<AdminError>)> {
    let user_id: Option<uuid::Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let is_admin = match user_id {
        Some(id) => User::find_by_id(state.db(), id)
            .await
            .ok()
            .flatten()
            .map(|u| u.is_admin)
            .unwrap_or(false),
        None => false,
    };
    if !is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(AdminError {
                error: "Admin access required".to_string(),
            }),
        ));
    }
    Ok(())
}

/// Switch the active stage for the current session.
///
/// POST /admin/stage/switch
async fn switch_stage(
    State(state): State<AppState>,
    session: Session,
    Json(request): Json<StageSwitchRequest>,
) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
    require_admin_json(&state, &session).await?;

    session
        .insert(SESSION_ACTIVE_STAGE, request.stage_id.clone())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to update active_stage in session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminError {
                    error: "Failed to switch stage".to_string(),
                }),
            )
        })?;

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
) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
    require_admin_json(&state, &session).await?;

    let active_stage: Option<String> = session
        .get(SESSION_ACTIVE_STAGE)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to get active_stage from session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminError {
                    error: "Failed to get stage".to_string(),
                }),
            )
        })?
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

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin");
    context.insert("user", &user);

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
        if !owners.contains_key(&file.owner_id.to_string()) {
            if let Ok(Some(user)) = User::find_by_id(state.db(), file.owner_id).await {
                owners.insert(file.owner_id.to_string(), user.name);
            }
        }
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

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

    let owner = User::find_by_id(state.db(), file.owner_id)
        .await
        .ok()
        .flatten();
    let public_url = state.files().storage().public_url(&file.uri);

    let mut context = tera::Context::new();
    context.insert("file", &file);
    context.insert("owner", &owner);
    context.insert("public_url", &public_url);
    context.insert("path", &format!("/admin/content/files/{}", file_id));

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
// AJAX Endpoint
// =============================================================================

/// AJAX form callback endpoint.
///
/// POST /system/ajax
async fn ajax_callback(
    State(state): State<AppState>,
    session: Session,
    Json(request): Json<AjaxRequest>,
) -> Response {
    use crate::form::AjaxResponse;
    use crate::tap::{RequestState, UserContext};

    // Require authentication for AJAX requests
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(AjaxResponse::new().alert("Session expired. Please log in again.")),
            )
                .into_response();
        }
    };

    // Handle admin-specific AJAX triggers
    if request.trigger == "add_field" {
        return super::admin_content_type::handle_ajax_add_field(&state, &request).await;
    }

    // Build user context with permissions
    let permissions = if user.is_admin {
        vec!["administer site".to_string()]
    } else {
        vec![]
    };
    let user_context = UserContext::authenticated(user.id, permissions);
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
        Comment::list_by_status(state.db(), status, per_page, offset)
            .await
            .unwrap_or_default()
    } else {
        Comment::list_all(state.db(), per_page, offset)
            .await
            .unwrap_or_default()
    };

    let total = Comment::count_all(state.db()).await.unwrap_or(0);

    // Get author names
    let mut authors: std::collections::HashMap<uuid::Uuid, String> =
        std::collections::HashMap::new();
    for comment in &comments {
        if !authors.contains_key(&comment.author_id) {
            if let Ok(Some(user)) = User::find_by_id(state.db(), comment.author_id).await {
                authors.insert(comment.author_id, user.name);
            }
        }
    }

    // Get item titles
    let mut items: std::collections::HashMap<uuid::Uuid, String> = std::collections::HashMap::new();
    for comment in &comments {
        if !items.contains_key(&comment.item_id) {
            if let Ok(Some(item)) = Item::find_by_id(state.db(), comment.item_id).await {
                items.insert(comment.item_id, item.title);
            }
        }
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("comments", &comments);
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

    let comment = match Comment::find_by_id(state.db(), id).await {
        Ok(Some(c)) => c,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load comment");
            return render_server_error("Failed to load comment");
        }
    };

    let author_name = User::find_by_id(state.db(), comment.author_id)
        .await
        .ok()
        .flatten()
        .map(|u| u.name);

    let item_title = Item::find_by_id(state.db(), comment.item_id)
        .await
        .ok()
        .flatten()
        .map(|i| i.title);

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("comment", &comment);
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let input = UpdateComment {
        body: Some(form.body),
        body_format: None,
        status: Some(form.status),
    };

    match Comment::update(state.db(), id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/content/comments").into_response(),
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update comment");
            render_server_error("Failed to update comment")
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let input = UpdateComment {
        body: None,
        body_format: None,
        status: Some(1),
    };

    match Comment::update(state.db(), id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/content/comments").into_response(),
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to approve comment");
            render_server_error("Failed to approve comment")
        }
    }
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let input = UpdateComment {
        body: None,
        body_format: None,
        status: Some(0),
    };

    match Comment::update(state.db(), id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/content/comments").into_response(),
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to unpublish comment");
            render_server_error("Failed to unpublish comment")
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match Comment::delete(state.db(), id).await {
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
        // Content type and search configuration management
        .merge(super::admin_content_type::router())
        // URL Alias management
        .merge(super::admin_alias::router())
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
        .route(
            "/admin/content/comments/{id}/delete",
            post(delete_comment_admin),
        )
}
