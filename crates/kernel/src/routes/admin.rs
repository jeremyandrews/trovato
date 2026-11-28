//! Core admin routes: dashboard, stage management, file management,
//! comment moderation, and AJAX callbacks.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::file::service::{FileStatus, MediaScope};
use crate::form::{AjaxRequest, FormElement, ValidationError};
use crate::models::stage::LIVE_STAGE_ID;
use crate::models::{Comment, CommentStatus, SiteConfig, UpdateComment};
use crate::routes::auth::SESSION_ACTIVE_STAGE;
use crate::state::AppState;

use crate::form::csrf::generate_csrf_token;

use crate::error::AppError;

use super::admin_form;
use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    admin_user_context, render_admin_template, render_error, render_not_found, render_server_error,
    require_permission,
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

/// The caller's context, if they may read stages other than Live; a 403
/// otherwise, anonymous visitors included.
async fn require_use_stages(
    state: &AppState,
    session: &Session,
) -> Result<crate::tap::UserContext, AppError> {
    let viewer = super::helpers::get_user_context(session, state).await;
    if viewer.authenticated && viewer.can(crate::content::USE_STAGES) {
        Ok(viewer)
    } else {
        Err(AppError::forbidden("You may not use stages"))
    }
}

/// Switch the active stage for the current session.
///
/// POST /admin/stage/switch
///
/// CSRF first, then `use stages` (an administrator holds it implicitly). A
/// `stage_id` of `null` or Live's id goes back to Live; any other value must
/// be the id of a stage that exists, or the request is a 400 and the session
/// keeps what it had. What the session's stage is used for is
/// [`super::helpers::read_stages`]'s decision, which asks the permission
/// again on every read.
async fn switch_stage(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(request): Json<StageSwitchRequest>,
) -> Result<Json<StageSwitchResponse>, AppError> {
    super::helpers::require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;

    require_use_stages(&state, &session).await?;

    let stage = match request.stage_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(value) => {
            let id = value
                .parse::<uuid::Uuid>()
                .map_err(|_| AppError::bad_request("stage_id must be a stage id"))?;
            let view = state
                .items()
                .stage_view()
                .await
                .map_err(|e| AppError::internal_ctx(e, "switch stage"))?;
            if view.stage(id).is_none() {
                return Err(AppError::bad_request("no stage has that id"));
            }
            (id != LIVE_STAGE_ID).then_some(id)
        }
    };
    let stored = stage.map(|id| id.to_string());

    session
        .insert(SESSION_ACTIVE_STAGE, stored.clone())
        .await
        .map_err(|e| AppError::internal_ctx(anyhow::anyhow!(e), "switch stage"))?;

    tracing::info!(stage = ?stored, "stage switched");

    Ok(Json(StageSwitchResponse {
        success: true,
        active_stage: stored,
    }))
}

/// Get the current active stage.
///
/// GET /admin/stage/current, for a holder of `use stages`.
async fn get_current_stage(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<StageSwitchResponse>, AppError> {
    require_use_stages(&state, &session).await?;

    let active_stage = super::helpers::session_stage(&session)
        .await
        .map(|id| id.to_string());

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
///
/// Gated on `access administration pages`, which is admission to the section and
/// nothing more. Every screen the dashboard links to still asks for its own
/// permission, so this page can be opened by a role that may use only one of
/// them — which is the point. Before this permission existed the dashboard took
/// `administer site`, so a role delegated `administer comments` could reach
/// `/admin/content/comments` by typing the address and got 403 on the page that
/// would have linked to it.
///
/// The links are filtered to what the viewer may actually open, so the page
/// never offers a door that answers 403.
async fn dashboard(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "access administration pages").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let viewer = admin_user_context(&state, &user).await;

    let content_types = state.content_types().list_all().await;
    // One entry per type the viewer may actually create, built from the same
    // string `/item/add/{type}` checks.
    let creatable: Vec<_> = content_types
        .iter()
        .filter(|ct| viewer.can(&format!("create {} content", ct.machine_name)))
        .cloned()
        .collect();

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("content_types", &creatable);
    // One card per administration area, each link under the gate and plugin
    // check the sidebar uses, so a viewer is offered no screen that answers
    // 403 or 404.
    let enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    let nav = state.plugin_admin_nav();
    let mut cards = super::admin_nav::dashboard(&viewer, &enabled, nav.links(), nav.cards());
    // Each group card's status lines, from its offered pages, skipping what
    // a panel below already reports; awaited one after another in this
    // request's own task, so the request profile counts every statement.
    let pages = super::admin_nav::offered_pages(&viewer, &enabled, &[], &[]);
    for card in &mut cards {
        let Some(group) = card.group else { continue };
        for page in pages.iter().filter(|p| p.group == Some(group)) {
            let Some(status) = page.status else { continue };
            if super::admin_nav::DASHBOARD_PANEL_STATUSES.contains(&status) {
                continue;
            }
            if let Some(line) = super::admin_nav::status_line(status, &viewer, &state).await {
                card.status_lines.push(line);
            }
        }
    }
    context.insert("dashboard_cards", &cards);
    // What needs the viewer now: each panel only for someone who may open
    // the page it is about.
    context.insert(
        "dashboard_panels",
        &super::admin_dashboard::panels(&state, &viewer).await,
    );
    context.insert("path", "/admin");
    context.insert("user", &user);
    context.insert("csrf_token", &csrf_token);

    // The update banner, read from what the last cron check stored. Only here,
    // and only for a viewer holding `administer site`: a visitor has no use for
    // the site's version and no business being told it, and neither has someone
    // admitted to the section to moderate comments — they cannot act on an
    // update, and whether a security release is outstanding is not theirs to
    // know. Keeping it on `administer site` leaves this page's disclosure
    // exactly where it was before the section opened wider. Nothing is fetched
    // on this path — a page render never makes an outbound request.
    if viewer.can("administer site") {
        if let Some(status) = crate::update_status::stored_status(state.db()).await
            && status.is_behind()
        {
            context.insert("update_status", &status);
        }
        context.insert("running_version", crate::update_status::running_version());
    }

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/dashboard.html",
        context,
    )
    .await
}

// =============================================================================
// File Management
// =============================================================================

/// The file listing's sortable columns: key and SQL.
const FILE_SORTS: &[(&str, &str)] = &[
    ("name", "lower(filename)"),
    ("type", "filemime"),
    ("size", "filesize"),
    ("status", "status"),
    ("uploaded", "created"),
];

/// List files, a page at a time, filtered by status and file name.
///
/// GET /admin/content/files
async fn list_files(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let user = match require_permission(&state, &session, "access files").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let status_filter = params.get("status").and_then(|s| match s.as_str() {
        "0" => Some(FileStatus::Temporary),
        "1" => Some(FileStatus::Permanent),
        _ => None,
    });

    let query = ListQuery::from_params(&params);
    let sort = query.sort(FILE_SORTS, ("uploaded", true));
    let like = query.like();

    let (files, total) = match state
        .files()
        .admin_page(
            status_filter,
            like.as_deref(),
            &sort.order_by(),
            PER_PAGE,
            query.offset(),
        )
        .await
    {
        Ok(page) => page,
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

    let list = query.view(total, files.len(), Some(&sort));
    let forms = match super::admin_list::render_forms(
        &state,
        &session,
        &user,
        &list,
        "/admin/content/files",
        "Filter files",
        "File name contains",
        vec![(
            "status",
            FormElement::select(vec![
                ("0".to_string(), "Temporary".to_string()),
                ("1".to_string(), "Permanent".to_string()),
            ])
            .title("Status")
            .placeholder("- Any -")
            .default_value(
                status_filter
                    .map(|s| (s as i16).to_string())
                    .unwrap_or_default(),
            ),
        )],
    )
    .await
    {
        Ok(forms) => forms,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    forms.insert_into(&mut context);
    context.insert("files", &files);
    context.insert("list", &list);
    context.insert("owners", &owners);
    context.insert("path", "/admin/content/files");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/files.html",
        context,
    )
    .await
}

/// The alternative text form on a file's details page.
const FILE_ALT_TEXT_FORM_ID: &str = "file_alt_text_form";

/// The form on the page that confirms deleting a file.
const FILE_DELETE_FORM_ID: &str = "file_delete_form";

/// The longest alternative text the form takes, the limit its field shows.
const ALT_TEXT_MAX_CHARS: usize = 1024;

/// A delete confirmation form: one danger button posting to `action`, with a
/// Cancel link beside it back to `cancel`, its word in the viewer's admin
/// language. `cancel` is a path this file writes, so it is printed as it is.
async fn delete_confirm_form(
    state: &AppState,
    user: &crate::models::User,
    form_id: &str,
    action: String,
    cancel: &'static str,
) -> crate::form::Form {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, cancel).await;
    crate::form::Form::new(form_id).action(action).element(
        "op",
        FormElement::submit("Delete")
            .attribute("class", "button--danger")
            .suffix(format!(
                r#"<a href="{}" class="button button--secondary">{}</a>"#,
                cancel,
                text.html("Cancel", &[])
            ))
            .weight(10),
    )
}

/// The alternative text form for `file`, showing `value`.
fn alt_text_form(file: &crate::file::service::FileInfo, value: &str) -> crate::form::Form {
    // With nothing recorded the hint says so first, so an image nobody has
    // described is not mistaken for one marked decorative.
    let hint = if file.alt_text.is_none() {
        "Not set. What the image shows, for readers who cannot see it. Leave empty only if the image is decorative and carries no information."
    } else {
        "What the image shows, for readers who cannot see it. Leave empty only if the image is decorative and carries no information."
    };
    crate::form::Form::new(FILE_ALT_TEXT_FORM_ID)
        .action(format!("/admin/content/files/{}/alt-text", file.id))
        .element(
            "alt_text",
            FormElement::textfield()
                .title("Alternative text")
                .description(hint)
                .default_value(value.to_string())
                .max_length(ALT_TEXT_MAX_CHARS)
                .weight(0),
        )
        .element("op", FormElement::submit("Save").weight(10))
}

/// Show file details.
///
/// GET /admin/content/files/{id}
async fn file_details(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "access files").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_file_details(&state, &session, &user, file_id, None, &[]).await
}

/// The file details page, its alternative text form showing `alt_value`
/// (the stored text when `None`) and `errors`.
async fn render_file_details(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    file_id: uuid::Uuid,
    alt_value: Option<&str>,
    errors: &[ValidationError],
) -> Response {
    let Some(file) = state.files().get(file_id).await.ok().flatten() else {
        return render_not_found();
    };

    let owner = state.users().find_by_id(file.owner_id).await.ok().flatten();
    let public_url = state.files().storage().public_url(&file.uri);
    let value = alt_value.unwrap_or_else(|| file.alt_text.as_deref().unwrap_or_default());
    let form_html =
        match admin_form::render(state, session, user, alt_text_form(&file, value), errors).await {
            Ok(html) => html,
            Err(response) => return response,
        };

    let mut context = tera::Context::new();
    context.insert("file", &file);
    context.insert("owner", &owner);
    context.insert("public_url", &public_url);
    context.insert("alt_text_form", &form_html);
    context.insert("path", &format!("/admin/content/files/{file_id}"));

    render_admin_template(
        state,
        &super::helpers::user_context_for(state, user).await,
        "admin/file-details.html",
        context,
    )
    .await
}

/// Confirm deleting a file.
///
/// GET /admin/content/files/{id}/delete
async fn delete_file_form(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "administer files").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let Some(file) = state.files().get(file_id).await.ok().flatten() else {
        return render_not_found();
    };
    let path = format!("/admin/content/files/{file_id}/delete");
    let form = delete_confirm_form(
        &state,
        &user,
        FILE_DELETE_FORM_ID,
        path.clone(),
        "/admin/content/files",
    )
    .await;
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("file", &file);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(
        &state,
        &super::helpers::user_context_for(&state, &user).await,
        "admin/file-delete.html",
        context,
    )
    .await
}

/// Delete a file.
///
/// POST /admin/content/files/{id}/delete
async fn delete_file(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer files").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        FILE_DELETE_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
        Err(Err(response)) => return response,
    };

    match state.files().delete(file_id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
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

/// Set a file's alternative text.
///
/// POST /admin/content/files/{id}/alt-text
///
/// An empty value means "explicitly decorative", which is a real answer
/// rather than an absent one. An optional `redirect_to` names where to return
/// to, so the media library and the file details page can share one endpoint
/// without one of them throwing the user to the other.
async fn set_file_alt_text(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer files").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let alt_text = posted.get("alt_text").unwrap_or_default().to_string();
    let mut errors = Vec::new();
    if alt_text.chars().count() > ALT_TEXT_MAX_CHARS {
        errors.push(ValidationError::field(
            "alt_text",
            "Alternative text must be at most 1024 characters.",
        ));
    }
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        FILE_ALT_TEXT_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_file_details(&state, &session, &user, file_id, Some(&alt_text), &errors)
                .await;
        }
        Err(Err(response)) => return response,
    };

    match state.files().set_alt_text(file_id, Some(&alt_text)).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            // Only a same-site path is honoured, so the return target cannot be
            // turned into an open redirect by a crafted form post.
            let target = posted
                .get("redirect_to")
                .filter(|path| path.starts_with('/') && !path.starts_with("//"))
                .map_or_else(|| format!("/admin/content/files/{file_id}"), str::to_string);
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
    let user = match require_permission(&state, &session, "access files").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let type_filter = params.get("type").map(String::as_str);
    // "All files" is the type select's default, not a filter: left in the
    // query it would make every submitted form count as filtered.
    let mut list_params = params.clone();
    if type_filter == Some("all") {
        list_params.remove("type");
    }
    let query = ListQuery::from_params(&list_params);
    let search = Some(query.q.as_str()).filter(|q| !q.is_empty());

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
            MediaScope::All,
            "newest",
            PER_PAGE,
            query.offset(),
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
        .count_filtered_media(
            Some(FileStatus::Permanent),
            mime_prefix,
            search,
            MediaScope::All,
        )
        .await
        .unwrap_or(0);

    // Build public URLs for each file
    let storage = state.files().storage().clone();
    let file_urls: std::collections::HashMap<String, String> = files
        .iter()
        .map(|f| (f.id.to_string(), storage.public_url(&f.uri)))
        .collect();

    let list = query.view(total, files.len(), None);
    // "All files" is the type select's empty choice; an older address's
    // `type=all` still reads as no type.
    let type_value = type_filter
        .filter(|t| matches!(*t, "image" | "document"))
        .unwrap_or_default();
    let forms = match super::admin_list::render_forms(
        &state,
        &session,
        &user,
        &list,
        "/admin/media",
        "Filter media",
        "File name contains",
        vec![(
            "type",
            FormElement::select(vec![
                ("image".to_string(), "Images".to_string()),
                ("document".to_string(), "Documents".to_string()),
            ])
            .title("Type")
            .placeholder("All files")
            .default_value(type_value),
        )],
    )
    .await
    {
        Ok(forms) => forms,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    forms.insert_into(&mut context);
    context.insert("files", &files);
    context.insert("file_urls", &file_urls);
    context.insert("list", &list);
    context.insert("path", "/admin/media");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/media-library.html",
        context,
    )
    .await
}

// =============================================================================
// AJAX Endpoint
// =============================================================================

/// AJAX form callback endpoint.
///
/// POST /system/ajax
///
/// Every request carries the form's AJAX token in `X-CSRF-Token`; tokens are
/// single use, so a request that passes gets the next one in the same response
/// header. `add_field` is the content type field screen's trigger and stays
/// with `administer site`; every other trigger is the form pipeline's
/// ([`super::item_form::ajax`]), held to whoever may open the form.
async fn ajax_callback(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(request): Json<AjaxRequest>,
) -> Response {
    // Who is asking comes before whether the request is forged, so nobody
    // gets the 401 the refusal rule promises rather than a CSRF 403.
    let mut admin_lang = state.default_language().to_string();
    if request.trigger == "add_field" {
        match super::helpers::require_permission_json(&state, &session, "administer site").await {
            Ok(user) => {
                let prefs = super::admin_nav::Preferences::load(state.db(), user.id).await;
                admin_lang = super::admin_nav::admin_language(&state, &prefs);
            }
            Err(refusal) => return refusal.into_response(),
        }
    }
    if let Err(refusal) = super::helpers::require_csrf_header(&session, &headers).await {
        return refusal.into_response();
    }

    let mut response = if request.trigger == "add_field" {
        super::admin_content_type::handle_ajax_add_field(&state, &request, &admin_lang).await
    } else {
        super::item_form::ajax(&state, &session, &request).await
    };

    let next = crate::form::csrf::generate_csrf_token(&session).await;
    if let Ok(value) = axum::http::HeaderValue::from_str(&next) {
        response.headers_mut().insert("X-CSRF-Token", value);
    }
    response
}

// =============================================================================
// Comment Moderation
// =============================================================================

/// The form the moderation list's approve, unpublish and mark-as-spam buttons
/// belong to. One form serves the whole table: each button names it with the
/// `form` attribute and posts to its own action with `formaction`, so the
/// page builds one form rather than one per row.
const COMMENT_ACTIONS_FORM_ID: &str = "comment_actions_form";

/// The comment settings form under the moderation list.
const COMMENT_SETTINGS_FORM_ID: &str = "comment_settings_form";

/// The admin comment edit form.
const COMMENT_EDIT_FORM_ID: &str = "comment_edit_form";

/// The form on the page that confirms deleting a comment.
const COMMENT_DELETE_FORM_ID: &str = "comment_delete_form";

/// What the comment settings form shows: the default status for new comments
/// (`published` or `pending`) and whether content authors are e-mailed.
struct CommentSettingsValues {
    /// `published` or `pending`, or what was posted when it was neither.
    default_status: String,
    /// The "E-mail content authors about new comments" checkbox.
    notify_authors: bool,
}

/// The comment settings form showing `values`.
fn comment_settings_form(values: &CommentSettingsValues) -> crate::form::Form {
    crate::form::Form::new(COMMENT_SETTINGS_FORM_ID)
        .action("/admin/content/comments/settings")
        .element(
            "default_status",
            FormElement::select(vec![
                ("published".to_string(), "Publish immediately".to_string()),
                ("pending".to_string(), "Hold for review".to_string()),
            ])
            .title("New comments")
            .description(
                "Whether a comment posted from now on is shown at once or waits in this list for approval. Comments already posted keep their status.",
            )
            .default_value(values.default_status.clone())
            .required()
            .weight(0),
        )
        .element(
            "notify_authors",
            FormElement::checkbox()
                .title("E-mail content authors about new comments")
                .description(
                    "When ticked, the author of a post is e-mailed when a comment on it is published or approved. Their own comments never are.",
                )
                .default_value(values.notify_authors)
                .weight(1),
        )
        .element(
            "op",
            FormElement::submit("Save")
                .attribute("class", "button--secondary")
                .weight(10),
        )
}

/// What the comment edit form shows.
struct CommentEditValues {
    /// The subject, empty for none.
    subject: String,
    /// The body.
    body: String,
    /// The status, as its stored number.
    status: String,
}

/// The comment edit form for comment `id`, showing `values`, with a Cancel
/// link beside Save in the viewer's admin language.
async fn comment_edit_form(
    state: &AppState,
    user: &crate::models::User,
    id: uuid::Uuid,
    values: &CommentEditValues,
) -> crate::form::Form {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text =
        super::admin_nav::AdminText::for_page(state, &viewer, "/admin/content/comments").await;
    // Options come from CommentStatus, so this form offers every status the
    // model stores and labels each one the way the moderation list does.
    let statuses = COMMENT_STATUS_FILTERS
        .iter()
        .map(|s| (s.as_i16().to_string(), s.label().to_string()))
        .collect();
    crate::form::Form::new(COMMENT_EDIT_FORM_ID)
        .action(format!("/admin/content/comments/{id}/edit"))
        .element(
            "subject",
            FormElement::textfield()
                .title("Subject")
                .default_value(values.subject.clone())
                .max_length(crate::models::comment::SUBJECT_MAX_CHARS)
                .weight(0),
        )
        .element(
            "body",
            FormElement::textarea(6)
                .title("Comment Body")
                .default_value(values.body.clone())
                .required()
                .weight(1),
        )
        .element(
            "status",
            FormElement::select(statuses)
                .title("Status")
                .default_value(values.status.clone())
                .required()
                .weight(2),
        )
        .element(
            "op",
            FormElement::submit("Save")
                .suffix(format!(
                    r#"<a href="/admin/content/comments" class="button button--secondary">{}</a>"#,
                    text.html("Cancel", &[])
                ))
                .weight(10),
        )
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

/// The comment listing's status filter choices, as value and English label.
/// They come from `CommentStatus`, so the filter cannot drift from the
/// statuses the model stores.
fn comment_status_options() -> Vec<(String, String)> {
    COMMENT_STATUS_FILTERS
        .iter()
        .map(|s| (s.as_i16().to_string(), s.label().to_string()))
        .collect()
}

/// The statuses the moderation screens offer, in display order.
const COMMENT_STATUS_FILTERS: [CommentStatus; 4] = [
    CommentStatus::Pending,
    CommentStatus::Published,
    CommentStatus::Unpublished,
    CommentStatus::Spam,
];

/// The comment listing's sortable columns: key and SQL (`c` is the comment,
/// `u` its author).
const COMMENT_SORTS: &[(&str, &str)] = &[
    ("author", "lower(u.name)"),
    ("status", "c.status"),
    ("posted", "c.created"),
];

/// List comments for moderation, a page at a time, filtered by status and by
/// subject, body or author.
///
/// GET /admin/content/comments
async fn list_comments(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let user = match require_permission(&state, &session, "administer comments").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_comments(&state, &session, &user, &params, None, &[]).await
}

/// The moderation list for the query `params`, its settings form showing
/// `settings` (the stored settings when `None`) and `settings_errors`.
async fn render_comments(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    params: &std::collections::HashMap<String, String>,
    settings: Option<CommentSettingsValues>,
    settings_errors: &[ValidationError],
) -> Response {
    let status_filter = params.get("status").and_then(|s| s.parse::<i16>().ok());
    let query = ListQuery::from_params(params);
    let sort = query.sort(COMMENT_SORTS, ("posted", true));
    let like = query.like();

    let (comments, total) = match state
        .comments()
        .admin_page(
            status_filter,
            like.as_deref(),
            &sort.order_by(),
            PER_PAGE,
            query.offset(),
        )
        .await
    {
        Ok(page) => page,
        Err(e) => {
            tracing::error!(error = %e, "failed to list comments");
            return render_server_error("Failed to load comments.");
        }
    };

    // Get author names
    let mut authors: std::collections::HashMap<uuid::Uuid, String> =
        std::collections::HashMap::new();
    for comment in &comments {
        if !authors.contains_key(&comment.author_id)
            && let Ok(Some(author)) = state.users().find_by_id(comment.author_id).await
        {
            authors.insert(comment.author_id, author.name);
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

    let rows: Vec<CommentRow<'_>> = comments.iter().map(CommentRow::new).collect();

    // The row buttons' one form, built only when there are rows to act on.
    let actions_form = if rows.is_empty() {
        String::new()
    } else {
        let form =
            crate::form::Form::new(COMMENT_ACTIONS_FORM_ID).action("/admin/content/comments");
        match admin_form::render(state, session, user, form, &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        }
    };

    // The current default for new comments, so the screen that moderates
    // comments is also where the queue is turned on.
    let settings = match settings {
        Some(values) => values,
        None => CommentSettingsValues {
            default_status: if CommentStatus::default_for_new_comments(state.db())
                .await
                .awaits_review()
            {
                "pending".to_string()
            } else {
                "published".to_string()
            },
            notify_authors: crate::models::comment::notify_authors(state.db()).await,
        },
    };
    let settings_form = match admin_form::render(
        state,
        session,
        user,
        comment_settings_form(&settings),
        settings_errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };

    let list = query.view(total, rows.len(), Some(&sort));
    let forms = match super::admin_list::render_forms(
        state,
        session,
        user,
        &list,
        "/admin/content/comments",
        "Filter comments",
        "Comment or author contains",
        vec![(
            "status",
            FormElement::select(comment_status_options())
                .title("Status")
                .placeholder("- Any -")
                .default_value(status_filter.map(|s| s.to_string()).unwrap_or_default()),
        )],
    )
    .await
    {
        Ok(forms) => forms,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    forms.insert_into(&mut context);
    context.insert("comments", &rows);
    context.insert("actions_form", &actions_form);
    context.insert("settings_form", &settings_form);
    context.insert("authors", &authors);
    context.insert("items", &items);
    context.insert("list", &list);
    context.insert("path", "/admin/content/comments");

    render_admin_template(
        state,
        &super::helpers::user_context_for(state, user).await,
        "admin/comments.html",
        context,
    )
    .await
}

/// Edit a comment form.
///
/// GET /admin/content/comments/{id}/edit
async fn edit_comment_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "administer comments").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_comment_edit(&state, &session, &user, id, None, &[]).await
}

/// The comment edit page for comment `id`, its form showing `values` (the
/// stored comment when `None`) and `errors`.
async fn render_comment_edit(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    id: uuid::Uuid,
    values: Option<CommentEditValues>,
    errors: &[ValidationError],
) -> Response {
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

    let values = values.unwrap_or_else(|| CommentEditValues {
        subject: comment.subject.clone().unwrap_or_default(),
        body: comment.body.clone(),
        status: comment.status.to_string(),
    });
    let form = comment_edit_form(state, user, id, &values).await;
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("comment", &comment);
    context.insert("author_name", &author_name);
    context.insert("item_title", &item_title);
    context.insert("form", &form_html);
    context.insert("path", "/admin/content/comments");

    render_admin_template(
        state,
        &super::helpers::user_context_for(state, user).await,
        "admin/comment-form.html",
        context,
    )
    .await
}

/// Edit a comment submit.
///
/// POST /admin/content/comments/{id}/edit
async fn edit_comment_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer comments").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let values = CommentEditValues {
        subject: posted.get("subject").unwrap_or_default().to_string(),
        body: posted.get("body").unwrap_or_default().to_string(),
        status: posted.get("status").unwrap_or_default().to_string(),
    };
    let mut errors = Vec::new();
    // The form always carries the subject's full, current value, so empty
    // clears it rather than meaning "unchanged".
    let subject = match crate::routes::comment::validate_subject(Some(&values.subject)) {
        Ok(subject) => subject,
        Err(_) => {
            errors.push(ValidationError::field(
                "subject",
                format!(
                    "Subject cannot exceed {} characters",
                    crate::models::comment::SUBJECT_MAX_CHARS
                ),
            ));
            None
        }
    };
    if values.body.trim().is_empty() {
        errors.push(ValidationError::field("body", "Comment Body is required."));
    }
    // A status the model does not recognise is a malformed submission, not a new
    // state to store.
    let status = values
        .status
        .parse::<i16>()
        .ok()
        .filter(|s| CommentStatus::from_i16(*s).is_some());
    if status.is_none() {
        errors.push(ValidationError::field("status", "Unknown comment status"));
    }

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        COMMENT_EDIT_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_comment_edit(&state, &session, &user, id, Some(values), &errors).await;
        }
        Err(Err(response)) => return response,
    };
    let Some(status) = status else {
        return render_not_found();
    };

    let user_ctx = admin_user_context(&state, &user).await;
    let input = UpdateComment {
        body: Some(values.body),
        body_format: None,
        status: Some(status),
        subject: Some(subject),
    };

    // Approving out of the queue notifies the content author; re-saving an
    // already published comment does not.
    match update_comment_notifying(&state, id, input, &user_ctx).await {
        Ok(Some(_)) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to("/admin/content/comments").into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update comment");
            render_server_error("Failed to update comment")
        }
    }
}

/// Update a comment, queueing the content author's notification in the same
/// transaction when the update makes the comment visible (BG18).
///
/// The notification used to fire when a comment was created, which under a
/// hold-for-review default would have mailed the author the full text of every
/// comment the queue exists to catch. It belongs on the transition into
/// published, which is here and in the create route. One rule, in one place:
/// `should_notify_on_publish` decides, against the comment as it was inside the
/// transaction.
async fn update_comment_notifying(
    state: &AppState,
    id: uuid::Uuid,
    input: UpdateComment,
    user_ctx: &crate::tap::UserContext,
) -> anyhow::Result<Option<Comment>> {
    use crate::routes::comment::{
        comment_mail_context, comment_notification, should_notify_on_publish,
    };
    let before = state.comments().load(id).await.ok().flatten();
    let mut ctx = None;
    if let (Some(before), Some(status), Some(_)) = (&before, input.status, state.email())
        && let Ok(Some(item)) = state.items().load(before.item_id).await
        && should_notify_on_publish(
            Some(before.status),
            status,
            before.author_id,
            item.author_id,
        )
    {
        let commenter = state
            .users()
            .find_by_id(before.author_id)
            .await
            .ok()
            .flatten()
            .map(|u| u.name);
        ctx = comment_mail_context(state, &item, commenter.as_deref()).await;
    }
    match (state.email(), ctx) {
        (Some(email), Some(ctx)) => {
            let tera = state.theme().tera();
            state
                .comments()
                .update_with_mail(id, input, user_ctx, email, |before, c| {
                    comment_notification(tera, &ctx, c, before.map(|b| b.status))
                })
                .await
        }
        _ => state.comments().update(id, input, user_ctx).await,
    }
}

/// Set a comment's publication status (shared by approve, unpublish and
/// mark as spam), from a submission of the moderation list's row form.
async fn set_comment_status(
    state: &AppState,
    session: &Session,
    id: uuid::Uuid,
    posted: &PostedForm,
    status: i16,
    action: &str,
) -> Response {
    let user = match require_permission(state, session, "administer comments").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let form_state = match admin_form::check(
        state,
        session,
        &user,
        posted,
        COMMENT_ACTIONS_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
        Err(Err(response)) => return response,
    };

    let user_ctx = admin_user_context(state, &user).await;
    let input = UpdateComment {
        body: None,
        body_format: None,
        status: Some(status),
        subject: None,
    };

    match update_comment_notifying(state, id, input, &user_ctx).await {
        Ok(Some(_)) => {
            admin_form::submitted(state, &user, &form_state, posted).await;
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    set_comment_status(
        &state,
        &session,
        id,
        &PostedForm(pairs),
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    set_comment_status(
        &state,
        &session,
        id,
        &PostedForm(pairs),
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    set_comment_status(
        &state,
        &session,
        id,
        &PostedForm(pairs),
        CommentStatus::Spam.as_i16(),
        "mark as spam",
    )
    .await
}

/// The comment settings form.
///
/// GET /admin/content/comments/settings
///
/// The form is part of the moderation screen, so this serves that screen, with
/// its permission check; it used to be a POST-only route, and a bookmarked or
/// typed settings address answered 405.
async fn comment_settings_page(State(state): State<AppState>, session: Session) -> Response {
    list_comments(
        State(state),
        session,
        axum::extract::Query(std::collections::HashMap::new()),
    )
    .await
}

/// Set whether new comments are published immediately or held for review,
/// and whether content authors are e-mailed about new comments.
///
/// The setting lives on the moderation screen because that is where its
/// consequences are: turning the queue on fills the list below it.
///
/// POST /admin/content/comments/settings
async fn save_comment_settings(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer comments").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // The checkbox is posted as `1` when ticked, absent when not.
    let values = CommentSettingsValues {
        default_status: posted.get("default_status").unwrap_or_default().to_string(),
        notify_authors: posted.get("notify_authors").is_some(),
    };
    // Only the two statuses a new comment can meaningfully take.
    let value = match values.default_status.as_str() {
        "published" => Some("published"),
        "pending" => Some("pending"),
        _ => None,
    };
    let mut errors = Vec::new();
    if value.is_none() {
        tracing::warn!(value = %values.default_status, "rejected unknown comment default status");
        errors.push(ValidationError::field(
            "default_status",
            "Unknown comment status",
        ));
    }
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        COMMENT_SETTINGS_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_comments(
                &state,
                &session,
                &user,
                &std::collections::HashMap::new(),
                Some(values),
                &errors,
            )
            .await;
        }
        Err(Err(response)) => return response,
    };
    let Some(value) = value else {
        return render_not_found();
    };
    let notify_authors = values.notify_authors;

    let saved = async {
        SiteConfig::set(
            state.db(),
            crate::models::comment::DEFAULT_STATUS_KEY,
            serde_json::Value::String(value.to_string()),
        )
        .await?;
        SiteConfig::set(
            state.db(),
            crate::models::comment::NOTIFY_AUTHORS_KEY,
            serde_json::Value::Bool(notify_authors),
        )
        .await
    };
    match saved.await {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to("/admin/content/comments").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to save comment settings");
            render_server_error("Failed to save setting")
        }
    }
}

/// Confirm deleting a comment.
///
/// GET /admin/content/comments/{id}/delete
async fn delete_comment_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "administer comments").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let comment = match state.comments().load(id).await {
        Ok(Some(c)) => c,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load comment");
            return render_server_error("Failed to load comment");
        }
    };
    let path = format!("/admin/content/comments/{id}/delete");
    let form = delete_confirm_form(
        &state,
        &user,
        COMMENT_DELETE_FORM_ID,
        path.clone(),
        "/admin/content/comments",
    )
    .await;
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("comment", &comment);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(
        &state,
        &super::helpers::user_context_for(&state, &user).await,
        "admin/comment-delete.html",
        context,
    )
    .await
}

/// Delete a comment.
///
/// POST /admin/content/comments/{id}/delete
async fn delete_comment_admin(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer comments").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        COMMENT_DELETE_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
        Err(Err(response)) => return response,
    };

    let user_ctx = admin_user_context(&state, &user).await;
    match state.comments().delete(id, &user_ctx).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to("/admin/content/comments").into_response()
        }
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
        .route(
            "/admin/content/files/{id}/delete",
            get(delete_file_form).post(delete_file),
        )
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
        // The assistant, beside the other AI screens it shares a provider with.
        .merge(super::admin_ai_assistant::router())
        // Site configuration
        .merge(super::admin_config::router())
        // Clear all caches (B161)
        .merge(super::admin_performance::router())
        // Languages (B161)
        .merge(super::admin_language::router())
        // Search settings and the search report
        .merge(super::admin_search::router())
        .merge(super::admin_shell::router())
        .merge(super::admin_search_report::router())
        // The cron report and the site log (B107)
        .merge(super::admin_reports::router())
        .merge(super::admin_timers::router())
        .merge(super::admin_who_can::router())
        // Access rules (B108)
        .merge(super::admin_access_rules::router())
        // Text formats (B108)
        .merge(super::admin_text_format::router())
        // Appearance: the default and admin themes (B108)
        .merge(super::admin_appearance::router())
        // Plugin-queue dead-letter admin (P11d / D-46)
        // Async auto-embed lifecycle admin (P11f / D-51, D-52)
        .merge(super::admin_embed::router())
        // AJAX endpoint
        .route("/system/ajax", post(ajax_callback))
        // The confirmation page for destructive actions
        .merge(super::admin_confirm::router())
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
}

/// Category and tag admin routes (registered when "categories" plugin is enabled).
pub fn category_admin_router() -> Router<AppState> {
    super::admin_taxonomy::router()
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
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
            get(comment_settings_page).post(save_comment_settings),
        )
        .route(
            "/admin/content/comments/{id}/delete",
            get(delete_comment_form).post(delete_comment_admin),
        )
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
}
