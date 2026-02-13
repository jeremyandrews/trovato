//! Admin routes for content type management and site configuration.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::form::{generate_csrf_token, verify_csrf_token, AjaxCommand, AjaxRequest};
use crate::models::User;
use crate::routes::auth::{SESSION_ACTIVE_STAGE, SESSION_USER_ID};
use crate::state::AppState;

/// Stage switch request.
#[derive(Debug, Deserialize)]
pub struct StageSwitchRequest {
    /// Stage ID to switch to. None means "live" (production).
    pub stage_id: Option<String>,
}

/// Stage switch response.
#[derive(Debug, Serialize)]
pub struct StageSwitchResponse {
    pub success: bool,
    pub active_stage: Option<String>,
}

/// Error response.
#[derive(Debug, Serialize)]
pub struct AdminError {
    pub error: String,
}

/// Switch the active stage for the current session.
///
/// POST /admin/stage/switch
async fn switch_stage(
    session: Session,
    Json(request): Json<StageSwitchRequest>,
) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
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
    session: Session,
) -> Result<Json<StageSwitchResponse>, (StatusCode, Json<AdminError>)> {
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

/// Check if user is authenticated, return user or redirect to login.
async fn require_auth(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<uuid::Uuid> = session
        .get(SESSION_USER_ID)
        .await
        .ok()
        .flatten();

    if let Some(id) = user_id {
        if let Ok(Some(user)) = User::find_by_id(state.db(), id).await {
            return Ok(user);
        }
    }

    Err(Redirect::to("/user/login").into_response())
}

/// Admin dashboard.
///
/// GET /admin
async fn dashboard(
    State(state): State<AppState>,
    session: Session,
) -> Response {
    let user = match require_auth(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin");
    context.insert("user", &user);

    render_admin_template(&state, "admin/dashboard.html", &context).await
}

// =============================================================================
// Content Type Management
// =============================================================================

/// Content type form data.
#[derive(Debug, Deserialize)]
pub struct ContentTypeFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub label: String,
    pub machine_name: String,
    pub description: Option<String>,
    pub title_label: Option<String>,
    pub published_default: Option<String>,
    pub revision_default: Option<String>,
}

/// Field form data.
#[derive(Debug, Deserialize)]
pub struct FieldFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub label: String,
    pub name: String,
    pub field_type: String,
}

/// List all content types.
///
/// GET /admin/structure/types
async fn list_content_types(
    State(state): State<AppState>,
    session: Session,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/structure/types");

    render_admin_template(&state, "admin/content-types.html", &context).await
}

/// Show add content type form.
///
/// GET /admin/structure/types/add
async fn add_content_type_form(
    State(state): State<AppState>,
    session: Session,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/types/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/structure/types/add");

    render_admin_template(&state, "admin/content-type-form.html", &context).await
}

/// Handle add content type form submission.
///
/// POST /admin/structure/types/add
async fn add_content_type_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ContentTypeFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error(&state, "Invalid or expired form token. Please try again.").await;
    }

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if form.machine_name.trim().is_empty() {
        errors.push("Machine name is required.".to_string());
    } else if !is_valid_machine_name(&form.machine_name) {
        errors.push("Machine name must start with a letter and contain only lowercase letters, numbers, and underscores.".to_string());
    }

    // Check if machine name already exists
    if state.content_types().get(&form.machine_name).is_some() {
        errors.push(format!("A content type with machine name '{}' already exists.", form.machine_name));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/structure/types/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        context.insert("values", &serde_json::json!({
            "label": form.label,
            "machine_name": form.machine_name,
            "description": form.description,
            "title_label": form.title_label,
            "published_default": form.published_default.is_some(),
            "revision_default": form.revision_default.is_some(),
        }));
        context.insert("path", "/admin/structure/types/add");

        return render_admin_template(&state, "admin/content-type-form.html", &context).await;
    }

    // Create the content type
    let settings = serde_json::json!({
        "title_label": form.title_label.unwrap_or_else(|| "Title".to_string()),
        "published_default": form.published_default.is_some(),
        "revision_default": form.revision_default.is_some(),
    });

    match state
        .content_types()
        .create(
            &form.machine_name,
            &form.label,
            form.description.as_deref(),
            settings,
        )
        .await
    {
        Ok(_) => {
            tracing::info!(machine_name = %form.machine_name, "content type created");
            Redirect::to("/admin/structure/types").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create content type");
            render_error(&state, "Failed to create content type.").await
        }
    }
}

/// Show edit content type form.
///
/// GET /admin/structure/types/{type}/edit
async fn edit_content_type_form(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found(&state).await;
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/structure/types/{}/edit", type_name));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("values", &serde_json::json!({
        "label": content_type.label,
        "machine_name": content_type.machine_name,
        "description": content_type.description,
        "title_label": "Title",
        "published_default": false,
        "revision_default": false,
    }));
    context.insert("path", &format!("/admin/structure/types/{}/edit", type_name));

    render_admin_template(&state, "admin/content-type-form.html", &context).await
}

/// Handle edit content type form submission.
///
/// POST /admin/structure/types/{type}/edit
async fn edit_content_type_submit(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(form): Form<ContentTypeFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error(&state, "Invalid or expired form token. Please try again.").await;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found(&state).await;
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/structure/types/{}/edit", type_name));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("errors", &errors);
        context.insert("values", &serde_json::json!({
            "label": form.label,
            "machine_name": form.machine_name,
            "description": form.description,
            "title_label": form.title_label,
            "published_default": form.published_default.is_some(),
            "revision_default": form.revision_default.is_some(),
        }));
        context.insert("path", &format!("/admin/structure/types/{}/edit", type_name));

        return render_admin_template(&state, "admin/content-type-form.html", &context).await;
    }

    // Update the content type
    let settings = serde_json::json!({
        "title_label": form.title_label.unwrap_or_else(|| "Title".to_string()),
        "published_default": form.published_default.is_some(),
        "revision_default": form.revision_default.is_some(),
    });

    match state
        .content_types()
        .update(&type_name, &form.label, form.description.as_deref(), settings)
        .await
    {
        Ok(_) => {
            tracing::info!(machine_name = %type_name, "content type updated");
            Redirect::to("/admin/structure/types").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update content type");
            render_error(&state, "Failed to update content type.").await
        }
    }
}

/// Show manage fields page.
///
/// GET /admin/structure/types/{type}/fields
async fn manage_fields(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    use crate::form::FormState;

    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found(&state).await;
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    // Save initial form state for AJAX callbacks
    let form_state = FormState::new(
        format!("manage_fields_{}", type_name),
        form_build_id.clone(),
    );

    if let Err(e) = state.forms().save_state(&form_build_id, &form_state).await {
        tracing::warn!(error = %e, "failed to save initial form state");
    }

    let mut context = tera::Context::new();
    context.insert("content_type", &content_type);
    context.insert("fields", &content_type.fields);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("path", &format!("/admin/structure/types/{}/fields", type_name));

    render_admin_template(&state, "admin/field-list.html", &context).await
}

/// Add a field to a content type.
///
/// POST /admin/structure/types/{type}/fields/add
async fn add_field(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(form): Form<FieldFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error(&state, "Invalid or expired form token. Please try again.").await;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found(&state).await;
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Label is required.".to_string());
    }

    if form.name.trim().is_empty() {
        errors.push("Machine name is required.".to_string());
    } else if !is_valid_machine_name(&form.name) {
        errors.push("Machine name must start with a letter and contain only lowercase letters, numbers, and underscores.".to_string());
    }

    if form.field_type.is_empty() {
        errors.push("Field type is required.".to_string());
    }

    if !errors.is_empty() {
        // Return with errors - for now, just redirect back
        return Redirect::to(&format!("/admin/structure/types/{}/fields", type_name)).into_response();
    }

    // Add the field
    match state
        .content_types()
        .add_field(&type_name, &form.name, &form.label, &form.field_type)
        .await
    {
        Ok(_) => {
            tracing::info!(
                content_type = %type_name,
                field = %form.name,
                "field added"
            );
            Redirect::to(&format!("/admin/structure/types/{}/fields", type_name)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to add field");
            render_error(&state, "Failed to add field.").await
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
    let user = match require_auth(&state, &session).await {
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
        return handle_ajax_add_field(&state, &request).await;
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

/// Handle AJAX add_field trigger for manage_fields forms.
async fn handle_ajax_add_field(
    state: &AppState,
    request: &AjaxRequest,
) -> Response {
    use crate::form::AjaxResponse;

    // Load form state to get the content type name
    let form_state = match state.forms().load_state(&request.form_build_id).await {
        Ok(Some(fs)) => fs,
        Ok(None) => {
            return Json(AjaxResponse::new().alert("Form session expired. Please reload the page."))
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load form state");
            return Json(AjaxResponse::new().alert("An error occurred. Please try again."))
                .into_response();
        }
    };

    // Extract content type name from form_id (format: "manage_fields_{type}")
    let type_name = form_state
        .form_id
        .strip_prefix("manage_fields_")
        .unwrap_or(&form_state.form_id);

    // Get field values from request
    let label = request
        .values
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let name = request
        .values
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let field_type = request
        .values
        .get("field_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    // Validate
    if label.is_empty() {
        return Json(AjaxResponse::new().alert("Label is required.")).into_response();
    }
    if name.is_empty() {
        return Json(AjaxResponse::new().alert("Machine name is required.")).into_response();
    }
    if !is_valid_machine_name(name) {
        return Json(AjaxResponse::new().alert(
            "Machine name must start with a letter and contain only lowercase letters, numbers, and underscores.",
        ))
        .into_response();
    }
    if field_type.is_empty() {
        return Json(AjaxResponse::new().alert("Field type is required.")).into_response();
    }

    // Add the field
    if let Err(e) = state
        .content_types()
        .add_field(type_name, name, label, field_type)
        .await
    {
        tracing::error!(error = %e, "failed to add field via AJAX");
        return Json(AjaxResponse::new().alert("Failed to add field.")).into_response();
    }

    tracing::info!(content_type = %type_name, field = %name, "field added via AJAX");

    // Build the new row HTML
    let row_html = format!(
        r#"<tr data-field="{}">
            <td>{}</td>
            <td><code>{}</code></td>
            <td>{}</td>
            <td>No</td>
            <td>
                <a href="/admin/structure/types/{}/fields/{}/edit">Edit</a>
                &middot;
                <a href="/admin/structure/types/{}/fields/{}/delete"
                   onclick="return confirm('Are you sure you want to delete this field?')">Delete</a>
            </td>
        </tr>"#,
        html_escape(name),
        html_escape(label),
        html_escape(name),
        html_escape(field_type),
        html_escape(type_name),
        html_escape(name),
        html_escape(type_name),
        html_escape(name),
    );

    // Return AJAX response to append row and reset form
    Json(
        AjaxResponse::new()
            .append("#fields-tbody", row_html)
            .invoke(
                "Trovato.resetAddFieldForm",
                serde_json::json!({}),
            )
            .command(AjaxCommand::Remove {
                selector: "#no-fields-message".to_string(),
            }),
    )
    .into_response()
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Validate machine name format.
fn is_valid_machine_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let mut chars = name.chars();

    // First character must be lowercase letter
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }

    // Rest must be lowercase letters, digits, or underscores
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Render an admin template.
async fn render_admin_template(
    state: &AppState,
    template: &str,
    context: &tera::Context,
) -> Response {
    match state.theme().tera().render(template, context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template = %template, "failed to render template");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(format!(
                    r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body><h1>Template Error</h1><pre>{}</pre></body></html>"#,
                    html_escape(&e.to_string())
                )),
            )
                .into_response()
        }
    }
}

/// Render an error page.
async fn render_error(state: &AppState, message: &str) -> Response {
    let mut context = tera::Context::new();
    context.insert("message", message);
    context.insert("path", "/admin");

    let html = format!(
        r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body>
<div style="max-width: 600px; margin: 100px auto; text-align: center;">
<h1>Error</h1>
<p>{}</p>
<p><a href="javascript:history.back()">Go back</a></p>
</div>
</body></html>"#,
        html_escape(message)
    );

    (StatusCode::BAD_REQUEST, Html(html)).into_response()
}

/// Render a 404 page.
async fn render_not_found(state: &AppState) -> Response {
    let html = r#"<!DOCTYPE html>
<html><head><title>Not Found</title></head>
<body>
<div style="max-width: 600px; margin: 100px auto; text-align: center;">
<h1>Not Found</h1>
<p>The requested page could not be found.</p>
<p><a href="/admin">Return to admin</a></p>
</div>
</body></html>"#;

    (StatusCode::NOT_FOUND, Html(html)).into_response()
}

/// Escape HTML characters.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Create the admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        // Dashboard
        .route("/admin", get(dashboard))
        // Stage management
        .route("/admin/stage/switch", post(switch_stage))
        .route("/admin/stage/current", get(get_current_stage))
        // Content type management
        .route("/admin/structure/types", get(list_content_types))
        .route("/admin/structure/types/add", get(add_content_type_form))
        .route("/admin/structure/types/add", post(add_content_type_submit))
        .route("/admin/structure/types/{type}/edit", get(edit_content_type_form))
        .route("/admin/structure/types/{type}/edit", post(edit_content_type_submit))
        .route("/admin/structure/types/{type}/fields", get(manage_fields))
        .route("/admin/structure/types/{type}/fields/add", post(add_field))
        // AJAX endpoint
        .route("/system/ajax", post(ajax_callback))
}
