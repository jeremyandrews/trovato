//! Admin routes for content type management and site configuration.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::file::service::FileStatus;
use crate::form::{AjaxCommand, AjaxRequest, generate_csrf_token, verify_csrf_token};
use crate::models::role::well_known::{ANONYMOUS_ROLE_ID, AUTHENTICATED_ROLE_ID};
use crate::models::user::ANONYMOUS_USER_ID;
use crate::models::{
    Category, Comment, CreateCategory, CreateItem, CreateTag, CreateUrlAlias, CreateUser, Item,
    Role, Tag, UpdateCategory, UpdateComment, UpdateTag, UpdateUrlAlias, UpdateUser, UrlAlias,
    User,
};
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
    let user_id: Option<uuid::Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

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
async fn dashboard(State(state): State<AppState>, session: Session) -> Response {
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

/// User form data.
#[derive(Debug, Deserialize)]
pub struct UserFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub name: String,
    pub mail: String,
    pub password: Option<String>,
    pub is_admin: Option<String>,
    pub status: Option<String>,
}

/// Role form data.
#[derive(Debug, Deserialize)]
pub struct RoleFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub name: String,
}

/// Permission form data (for permission matrix).
#[derive(Debug, Deserialize)]
pub struct PermissionFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    #[serde(flatten)]
    pub permissions: std::collections::HashMap<String, String>,
}

/// Content form data.
#[derive(Debug, Deserialize)]
pub struct ContentFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub title: String,
    pub status: Option<String>,
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, serde_json::Value>,
}

/// Category form data.
#[derive(Debug, Deserialize)]
pub struct CategoryFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub hierarchy: Option<String>,
}

/// Tag form data.
#[derive(Debug, Deserialize)]
pub struct TagFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub label: String,
    pub description: Option<String>,
    pub weight: Option<String>,
    pub parent_id: Option<String>,
}

/// List all content types.
///
/// GET /admin/structure/types
async fn list_content_types(State(state): State<AppState>, session: Session) -> Response {
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
async fn add_content_type_form(State(state): State<AppState>, session: Session) -> Response {
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
        errors.push(format!(
            "A content type with machine name '{}' already exists.",
            form.machine_name
        ));
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
        context.insert(
            "values",
            &serde_json::json!({
                "label": form.label,
                "machine_name": form.machine_name,
                "description": form.description,
                "title_label": form.title_label,
                "published_default": form.published_default.is_some(),
                "revision_default": form.revision_default.is_some(),
            }),
        );
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
    context.insert(
        "action",
        &format!("/admin/structure/types/{}/edit", type_name),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert(
        "values",
        &serde_json::json!({
            "label": content_type.label,
            "machine_name": content_type.machine_name,
            "description": content_type.description,
            "title_label": "Title",
            "published_default": false,
            "revision_default": false,
        }),
    );
    context.insert(
        "path",
        &format!("/admin/structure/types/{}/edit", type_name),
    );

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
        context.insert(
            "action",
            &format!("/admin/structure/types/{}/edit", type_name),
        );
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "label": form.label,
                "machine_name": form.machine_name,
                "description": form.description,
                "title_label": form.title_label,
                "published_default": form.published_default.is_some(),
                "revision_default": form.revision_default.is_some(),
            }),
        );
        context.insert(
            "path",
            &format!("/admin/structure/types/{}/edit", type_name),
        );

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
        .update(
            &type_name,
            &form.label,
            form.description.as_deref(),
            settings,
        )
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
    context.insert(
        "path",
        &format!("/admin/structure/types/{}/fields", type_name),
    );

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
        return Redirect::to(&format!("/admin/structure/types/{}/fields", type_name))
            .into_response();
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
// Search Configuration
// =============================================================================

/// Search field configuration form data.
#[derive(Debug, Deserialize)]
pub struct SearchConfigFormData {
    #[serde(rename = "_token")]
    pub token: String,
    #[serde(rename = "_form_build_id")]
    pub form_build_id: String,
    pub field_name: String,
    pub weight: String,
}

/// Manage search field configuration for a content type.
///
/// GET /admin/structure/types/{type}/search
async fn manage_search_config(
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

    // Get current search configs
    let search_configs = match state.search().list_field_configs(&type_name).await {
        Ok(configs) => configs,
        Err(e) => {
            tracing::error!(error = %e, "failed to list search configs");
            vec![]
        }
    };

    // Build a map of field_name -> weight for easy template access
    let config_map: std::collections::HashMap<String, char> = search_configs
        .iter()
        .map(|c| (c.field_name.clone(), c.weight))
        .collect();

    let mut context = tera::Context::new();
    context.insert("content_type", &content_type);
    context.insert("fields", &content_type.fields);
    context.insert("search_configs", &config_map);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert(
        "path",
        &format!("/admin/structure/types/{}/search", type_name),
    );

    render_admin_template(&state, "admin/search-config.html", &context).await
}

/// Add or update a search field configuration.
///
/// POST /admin/structure/types/{type}/search/add
async fn add_search_config(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(form): Form<SearchConfigFormData>,
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

    // Validate weight
    let weight = form.weight.chars().next().unwrap_or('C');
    if !['A', 'B', 'C', 'D'].contains(&weight) {
        return render_error(&state, "Invalid weight. Must be A, B, C, or D.").await;
    }

    // Configure the field
    match state
        .search()
        .configure_field(&type_name, &form.field_name, weight)
        .await
    {
        Ok(_) => {
            tracing::info!(
                content_type = %type_name,
                field = %form.field_name,
                weight = %weight,
                "search field configured"
            );
            Redirect::to(&format!("/admin/structure/types/{}/search", type_name)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to configure search field");
            render_error(&state, "Failed to configure search field.").await
        }
    }
}

/// Remove a search field configuration.
///
/// POST /admin/structure/types/{type}/search/{field}/delete
async fn remove_search_config(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found(&state).await;
    };

    match state
        .search()
        .remove_field_config(&type_name, &field_name)
        .await
    {
        Ok(_) => {
            tracing::info!(
                content_type = %type_name,
                field = %field_name,
                "search field config removed"
            );
            Redirect::to(&format!("/admin/structure/types/{}/search", type_name)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to remove search field config");
            render_error(&state, "Failed to remove search field configuration.").await
        }
    }
}

/// Reindex all content of a specific type.
///
/// POST /admin/structure/types/{type}/search/reindex
async fn reindex_content_type(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found(&state).await;
    };

    match state.search().reindex_bundle(&type_name).await {
        Ok(count) => {
            tracing::info!(
                content_type = %type_name,
                count = %count,
                "content type reindexed"
            );
            Redirect::to(&format!("/admin/structure/types/{}/search", type_name)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to reindex content type");
            render_error(&state, "Failed to reindex content.").await
        }
    }
}

// =============================================================================
// User Management
// =============================================================================

/// List all users.
///
/// GET /admin/people
async fn list_users(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let users = match User::list(state.db()).await {
        Ok(users) => users,
        Err(e) => {
            tracing::error!(error = %e, "failed to list users");
            return render_error(&state, "Failed to load users.").await;
        }
    };

    let mut context = tera::Context::new();
    context.insert("users", &users);
    context.insert("path", "/admin/people");

    render_admin_template(&state, "admin/users.html", &context).await
}

/// Show add user form.
///
/// GET /admin/people/add
async fn add_user_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/people/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/people/add");

    render_admin_template(&state, "admin/user-form.html", &context).await
}

/// Handle add user form submission.
///
/// POST /admin/people/add
async fn add_user_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<UserFormData>,
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

    if form.name.trim().is_empty() {
        errors.push("Username is required.".to_string());
    }

    if form.mail.trim().is_empty() {
        errors.push("Email is required.".to_string());
    }

    let password = form.password.as_deref().unwrap_or("");
    if password.is_empty() {
        errors.push("Password is required.".to_string());
    } else if password.len() < 8 {
        errors.push("Password must be at least 8 characters.".to_string());
    }

    // Check if username already exists
    if let Ok(Some(_)) = User::find_by_name(state.db(), &form.name).await {
        errors.push(format!("Username '{}' is already taken.", form.name));
    }

    // Check if email already exists
    if let Ok(Some(_)) = User::find_by_mail(state.db(), &form.mail).await {
        errors.push(format!("Email '{}' is already in use.", form.mail));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/people/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
                "mail": form.mail,
                "is_admin": form.is_admin.is_some(),
                "status": form.status.is_some(),
            }),
        );
        context.insert("path", "/admin/people/add");

        return render_admin_template(&state, "admin/user-form.html", &context).await;
    }

    // Create the user
    let input = CreateUser {
        name: form.name.clone(),
        password: password.to_string(),
        mail: form.mail.clone(),
        is_admin: form.is_admin.is_some(),
    };

    match User::create(state.db(), input).await {
        Ok(_) => {
            tracing::info!(name = %form.name, "user created");
            Redirect::to("/admin/people").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create user");
            render_error(&state, "Failed to create user.").await
        }
    }
}

/// Show edit user form.
///
/// GET /admin/people/{id}/edit
async fn edit_user_form(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(user) = User::find_by_id(state.db(), user_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/people/{}/edit", user_id));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("user_id", &user_id.to_string());
    context.insert(
        "values",
        &serde_json::json!({
            "name": user.name,
            "mail": user.mail,
            "is_admin": user.is_admin,
            "status": user.status == 1,
        }),
    );
    context.insert("path", &format!("/admin/people/{}/edit", user_id));

    render_admin_template(&state, "admin/user-form.html", &context).await
}

/// Handle edit user form submission.
///
/// POST /admin/people/{id}/edit
async fn edit_user_submit(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
    Form(form): Form<UserFormData>,
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

    let Some(existing_user) = User::find_by_id(state.db(), user_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    // Validate
    let mut errors = Vec::new();

    if form.name.trim().is_empty() {
        errors.push("Username is required.".to_string());
    }

    if form.mail.trim().is_empty() {
        errors.push("Email is required.".to_string());
    }

    // Check if new username is taken by someone else
    if form.name != existing_user.name {
        if let Ok(Some(_)) = User::find_by_name(state.db(), &form.name).await {
            errors.push(format!("Username '{}' is already taken.", form.name));
        }
    }

    // Check if new email is taken by someone else
    if form.mail != existing_user.mail {
        if let Ok(Some(_)) = User::find_by_mail(state.db(), &form.mail).await {
            errors.push(format!("Email '{}' is already in use.", form.mail));
        }
    }

    // Validate password if provided
    if let Some(ref password) = form.password {
        if !password.is_empty() && password.len() < 8 {
            errors.push("Password must be at least 8 characters.".to_string());
        }
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/people/{}/edit", user_id));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("user_id", &user_id.to_string());
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
                "mail": form.mail,
                "is_admin": form.is_admin.is_some(),
                "status": form.status.is_some(),
            }),
        );
        context.insert("path", &format!("/admin/people/{}/edit", user_id));

        return render_admin_template(&state, "admin/user-form.html", &context).await;
    }

    // Update the user
    let input = UpdateUser {
        name: Some(form.name.clone()),
        mail: Some(form.mail.clone()),
        is_admin: Some(form.is_admin.is_some()),
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        timezone: None,
        language: None,
        data: None,
    };

    match User::update(state.db(), user_id, input).await {
        Ok(_) => {
            // Update password if provided
            if let Some(ref password) = form.password {
                if !password.is_empty() {
                    if let Err(e) = User::update_password(state.db(), user_id, password).await {
                        tracing::error!(error = %e, "failed to update user password");
                        return render_error(&state, "Failed to update password.").await;
                    }
                }
            }

            tracing::info!(user_id = %user_id, "user updated");
            Redirect::to("/admin/people").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update user");
            render_error(&state, "Failed to update user.").await
        }
    }
}

/// Delete a user.
///
/// POST /admin/people/{id}/delete
async fn delete_user(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
) -> Response {
    let current_user = match require_auth(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Prevent deleting anonymous user
    if user_id == ANONYMOUS_USER_ID {
        return render_error(&state, "Cannot delete the anonymous user.").await;
    }

    // Prevent deleting yourself
    if user_id == current_user.id {
        return render_error(&state, "Cannot delete your own account.").await;
    }

    match User::delete(state.db(), user_id).await {
        Ok(true) => {
            tracing::info!(user_id = %user_id, "user deleted");
            Redirect::to("/admin/people").into_response()
        }
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete user");
            render_error(&state, "Failed to delete user.").await
        }
    }
}

// =============================================================================
// Role Management
// =============================================================================

/// List all roles.
///
/// GET /admin/people/roles
async fn list_roles(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let roles = match Role::list(state.db()).await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_error(&state, "Failed to load roles.").await;
        }
    };

    let mut context = tera::Context::new();
    context.insert("roles", &roles);
    context.insert("anonymous_role_id", &ANONYMOUS_ROLE_ID.to_string());
    context.insert("authenticated_role_id", &AUTHENTICATED_ROLE_ID.to_string());
    context.insert("path", "/admin/people/roles");

    render_admin_template(&state, "admin/roles.html", &context).await
}

/// Show add role form.
///
/// GET /admin/people/roles/add
async fn add_role_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/people/roles/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/people/roles/add");

    render_admin_template(&state, "admin/role-form.html", &context).await
}

/// Handle add role form submission.
///
/// POST /admin/people/roles/add
async fn add_role_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RoleFormData>,
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

    if form.name.trim().is_empty() {
        errors.push("Role name is required.".to_string());
    }

    // Check if role name already exists
    if let Ok(Some(_)) = Role::find_by_name(state.db(), &form.name).await {
        errors.push(format!("A role named '{}' already exists.", form.name));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/people/roles/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
            }),
        );
        context.insert("path", "/admin/people/roles/add");

        return render_admin_template(&state, "admin/role-form.html", &context).await;
    }

    match Role::create(state.db(), &form.name).await {
        Ok(_) => {
            tracing::info!(name = %form.name, "role created");
            Redirect::to("/admin/people/roles").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create role");
            render_error(&state, "Failed to create role.").await
        }
    }
}

/// Show edit role form.
///
/// GET /admin/people/roles/{id}/edit
async fn edit_role_form(
    State(state): State<AppState>,
    session: Session,
    Path(role_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(role) = Role::find_by_id(state.db(), role_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    let permissions = Role::get_permissions(state.db(), role_id)
        .await
        .unwrap_or_default();

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/people/roles/{}/edit", role_id));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("role_id", &role_id.to_string());
    context.insert("role_permissions", &permissions);
    context.insert(
        "values",
        &serde_json::json!({
            "name": role.name,
        }),
    );
    context.insert("path", &format!("/admin/people/roles/{}/edit", role_id));

    render_admin_template(&state, "admin/role-form.html", &context).await
}

/// Handle edit role form submission.
///
/// POST /admin/people/roles/{id}/edit
async fn edit_role_submit(
    State(state): State<AppState>,
    session: Session,
    Path(role_id): Path<uuid::Uuid>,
    Form(form): Form<RoleFormData>,
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

    let Some(existing_role) = Role::find_by_id(state.db(), role_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    // Validate
    let mut errors = Vec::new();

    if form.name.trim().is_empty() {
        errors.push("Role name is required.".to_string());
    }

    // Check if new name is taken by someone else
    if form.name != existing_role.name {
        if let Ok(Some(_)) = Role::find_by_name(state.db(), &form.name).await {
            errors.push(format!("A role named '{}' already exists.", form.name));
        }
    }

    if !errors.is_empty() {
        let permissions = Role::get_permissions(state.db(), role_id)
            .await
            .unwrap_or_default();
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/people/roles/{}/edit", role_id));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("role_id", &role_id.to_string());
        context.insert("role_permissions", &permissions);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
            }),
        );
        context.insert("path", &format!("/admin/people/roles/{}/edit", role_id));

        return render_admin_template(&state, "admin/role-form.html", &context).await;
    }

    match Role::update(state.db(), role_id, &form.name).await {
        Ok(_) => {
            tracing::info!(role_id = %role_id, "role updated");
            Redirect::to("/admin/people/roles").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update role");
            render_error(&state, "Failed to update role.").await
        }
    }
}

/// Delete a role.
///
/// POST /admin/people/roles/{id}/delete
async fn delete_role(
    State(state): State<AppState>,
    session: Session,
    Path(role_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Prevent deleting built-in roles
    if role_id == ANONYMOUS_ROLE_ID || role_id == AUTHENTICATED_ROLE_ID {
        return render_error(&state, "Cannot delete built-in roles.").await;
    }

    match Role::delete(state.db(), role_id).await {
        Ok(true) => {
            tracing::info!(role_id = %role_id, "role deleted");
            Redirect::to("/admin/people/roles").into_response()
        }
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete role");
            render_error(&state, "Failed to delete role.").await
        }
    }
}

/// Show permission matrix.
///
/// GET /admin/people/permissions
async fn permissions_matrix(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let roles = match Role::list(state.db()).await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_error(&state, "Failed to load roles.").await;
        }
    };

    // Get permissions for each role
    let mut role_permissions: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for role in &roles {
        let perms = Role::get_permissions(state.db(), role.id)
            .await
            .unwrap_or_default();
        role_permissions.insert(role.id.to_string(), perms);
    }

    // Define available permissions
    let available_permissions = vec![
        "administer site",
        "access content",
        "create content",
        "edit own content",
        "edit any content",
        "delete own content",
        "delete any content",
        "access user profiles",
        "administer users",
        "administer categories",
        "access files",
        "administer files",
    ];

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("roles", &roles);
    context.insert("role_permissions", &role_permissions);
    context.insert("available_permissions", &available_permissions);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("path", "/admin/people/permissions");

    render_admin_template(&state, "admin/permissions.html", &context).await
}

/// Save permission matrix.
///
/// POST /admin/people/permissions
async fn save_permissions(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<PermissionFormData>,
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

    let roles = match Role::list(state.db()).await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_error(&state, "Failed to load roles.").await;
        }
    };

    let available_permissions = vec![
        "administer site",
        "access content",
        "create content",
        "edit own content",
        "edit any content",
        "delete own content",
        "delete any content",
        "access user profiles",
        "administer users",
        "administer categories",
        "access files",
        "administer files",
    ];

    // Process form data - permissions are submitted as "perm_{role_id}_{permission}"
    for role in &roles {
        let current_perms = Role::get_permissions(state.db(), role.id)
            .await
            .unwrap_or_default();

        for permission in &available_permissions {
            let key = format!("perm_{}_{}", role.id, permission.replace(' ', "_"));
            let should_have = form.permissions.contains_key(&key);
            let has_now = current_perms.contains(&permission.to_string());

            if should_have && !has_now {
                if let Err(e) = Role::add_permission(state.db(), role.id, permission).await {
                    tracing::error!(error = %e, role_id = %role.id, permission = %permission, "failed to add permission");
                }
            } else if !should_have && has_now {
                if let Err(e) = Role::remove_permission(state.db(), role.id, permission).await {
                    tracing::error!(error = %e, role_id = %role.id, permission = %permission, "failed to remove permission");
                }
            }
        }
    }

    tracing::info!("permissions updated");
    Redirect::to("/admin/people/permissions").into_response()
}

// =============================================================================
// Content Management
// =============================================================================

/// List all content.
///
/// GET /admin/content
async fn list_content(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let type_filter = params.get("type").map(|s| s.as_str());
    let status_filter = params.get("status").and_then(|s| s.parse::<i16>().ok());

    let items =
        match Item::list_filtered(state.db(), type_filter, status_filter, None, 100, 0).await {
            Ok(items) => items,
            Err(e) => {
                tracing::error!(error = %e, "failed to list content");
                return render_error(&state, "Failed to load content.").await;
            }
        };

    // Get authors for display
    let mut authors: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in &items {
        if !authors.contains_key(&item.author_id.to_string()) {
            if let Ok(Some(user)) = User::find_by_id(state.db(), item.author_id).await {
                authors.insert(item.author_id.to_string(), user.name);
            }
        }
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("items", &items);
    context.insert("authors", &authors);
    context.insert("content_types", &content_types);
    context.insert("type_filter", &type_filter.unwrap_or(""));
    context.insert(
        "status_filter",
        &status_filter.map(|s| s.to_string()).unwrap_or_default(),
    );
    context.insert("path", "/admin/content");

    render_admin_template(&state, "admin/content-list.html", &context).await
}

/// Select content type before adding.
///
/// GET /admin/content/add
async fn select_content_type(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/content/add");

    render_admin_template(&state, "admin/content-add-select.html", &context).await
}

/// Show add content form.
///
/// GET /admin/content/add/{type}
async fn add_content_form(
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
    context.insert("action", &format!("/admin/content/add/{}", type_name));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("content_type", &content_type);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", &format!("/admin/content/add/{}", type_name));

    render_admin_template(&state, "admin/content-form.html", &context).await
}

/// Handle add content form submission.
///
/// POST /admin/content/add/{type}
async fn add_content_submit(
    State(state): State<AppState>,
    session: Session,
    resolved_lang: Option<Extension<crate::middleware::language::ResolvedLanguage>>,
    Path(type_name): Path<String>,
    Form(form): Form<ContentFormData>,
) -> Response {
    let user = match require_auth(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error(&state, "Invalid or expired form token. Please try again.").await;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found(&state).await;
    };

    // Build fields JSON from form data (excluding system fields)
    let mut fields_json = serde_json::Map::new();
    for (key, value) in &form.fields {
        if !key.starts_with('_') && key != "title" && key != "status" && key != "log" {
            fields_json.insert(key.clone(), value.clone());
        }
    }

    // Validate all fields before checking errors
    let mut errors = Vec::new();

    if form.title.trim().is_empty() {
        errors.push("Title is required.".to_string());
    }

    // Process compound fields: parse JSON string from hidden input
    errors.extend(crate::content::compound::process_compound_fields(
        &mut fields_json,
        &content_type.fields,
    ));

    // Validate required non-compound fields
    errors.extend(crate::content::compound::validate_required_fields(
        &fields_json,
        &content_type.fields,
    ));

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/content/add/{}", type_name));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("content_type", &content_type);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "title": form.title,
                "status": form.status.is_some(),
                "fields": fields_json,
            }),
        );
        context.insert("path", &format!("/admin/content/add/{}", type_name));

        return render_admin_template(&state, "admin/content-form.html", &context).await;
    }

    let input = CreateItem {
        item_type: type_name.clone(),
        title: form.title.clone(),
        author_id: user.id,
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        stage_id: None,
        language: Some(
            resolved_lang
                .map(|Extension(lang)| lang.0)
                .unwrap_or_else(|| state.default_language().to_string()),
        ),
        log: Some("Created via admin UI".to_string()),
    };

    match Item::create(state.db(), input).await {
        Ok(item) => {
            tracing::info!(item_id = %item.id, "content created");
            Redirect::to("/admin/content").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create content");
            render_error(&state, "Failed to create content.").await
        }
    }
}

/// Show edit content form.
///
/// GET /admin/content/{id}/edit
async fn edit_content_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(item) = Item::find_by_id(state.db(), item_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_error(&state, "Content type not found.").await;
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/content/{}/edit", item_id));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("item_id", &item_id.to_string());
    context.insert("content_type", &content_type);
    context.insert("item", &item);
    context.insert(
        "values",
        &serde_json::json!({
            "title": item.title,
            "status": item.status == 1,
            "fields": item.fields,
        }),
    );
    context.insert("path", &format!("/admin/content/{}/edit", item_id));

    render_admin_template(&state, "admin/content-form.html", &context).await
}

/// Handle edit content form submission.
///
/// POST /admin/content/{id}/edit
async fn edit_content_submit(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
    Form(form): Form<ContentFormData>,
) -> Response {
    let user = match require_auth(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error(&state, "Invalid or expired form token. Please try again.").await;
    }

    let Some(item) = Item::find_by_id(state.db(), item_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_error(&state, "Content type not found.").await;
    };

    // Validate
    let mut errors = Vec::new();

    if form.title.trim().is_empty() {
        errors.push("Title is required.".to_string());
    }

    // Build fields JSON from form data
    let mut fields_json = serde_json::Map::new();
    for (key, value) in &form.fields {
        if !key.starts_with('_') && key != "title" && key != "status" && key != "log" {
            fields_json.insert(key.clone(), value.clone());
        }
    }

    // Process compound fields: parse JSON string from hidden input
    errors.extend(crate::content::compound::process_compound_fields(
        &mut fields_json,
        &content_type.fields,
    ));

    // Validate required non-compound fields
    errors.extend(crate::content::compound::validate_required_fields(
        &fields_json,
        &content_type.fields,
    ));

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/content/{}/edit", item_id));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("item_id", &item_id.to_string());
        context.insert("content_type", &content_type);
        context.insert("item", &item);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "title": form.title,
                "status": form.status.is_some(),
                "fields": fields_json,
            }),
        );
        context.insert("path", &format!("/admin/content/{}/edit", item_id));

        return render_admin_template(&state, "admin/content-form.html", &context).await;
    }

    let input = crate::models::UpdateItem {
        title: Some(form.title.clone()),
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        log: Some("Updated via admin UI".to_string()),
    };

    match Item::update(state.db(), item_id, user.id, input).await {
        Ok(_) => {
            tracing::info!(item_id = %item_id, "content updated");
            Redirect::to("/admin/content").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update content");
            render_error(&state, "Failed to update content.").await
        }
    }
}

/// Delete content.
///
/// POST /admin/content/{id}/delete
async fn delete_content(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    match Item::delete(state.db(), item_id).await {
        Ok(true) => {
            tracing::info!(item_id = %item_id, "content deleted");
            Redirect::to("/admin/content").into_response()
        }
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete content");
            render_error(&state, "Failed to delete content.").await
        }
    }
}

// =============================================================================
// Category Management
// =============================================================================

/// List all categories.
///
/// GET /admin/structure/categories
async fn list_categories(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let categories = match Category::list(state.db()).await {
        Ok(categories) => categories,
        Err(e) => {
            tracing::error!(error = %e, "failed to list categories");
            return render_error(&state, "Failed to load categories.").await;
        }
    };

    // Get tag counts for each category
    let mut tag_counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for cat in &categories {
        let count = Tag::count_by_category(state.db(), &cat.id)
            .await
            .unwrap_or(0);
        tag_counts.insert(cat.id.clone(), count);
    }

    let mut context = tera::Context::new();
    context.insert("categories", &categories);
    context.insert("tag_counts", &tag_counts);
    context.insert("path", "/admin/structure/categories");

    render_admin_template(&state, "admin/categories.html", &context).await
}

/// Show add category form.
///
/// GET /admin/structure/categories/add
async fn add_category_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/categories/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({"hierarchy": 0}));
    context.insert("path", "/admin/structure/categories/add");

    render_admin_template(&state, "admin/category-form.html", &context).await
}

/// Handle add category form submission.
///
/// POST /admin/structure/categories/add
async fn add_category_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CategoryFormData>,
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

    if form.id.trim().is_empty() {
        errors.push("Machine name is required.".to_string());
    } else if !is_valid_machine_name(&form.id) {
        errors.push("Machine name must start with a letter and contain only lowercase letters, numbers, and underscores.".to_string());
    }

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    // Check if category already exists
    if Category::exists(state.db(), &form.id)
        .await
        .unwrap_or(false)
    {
        errors.push(format!("A category with ID '{}' already exists.", form.id));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/structure/categories/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "id": form.id,
                "label": form.label,
                "description": form.description,
                "hierarchy": form.hierarchy,
            }),
        );
        context.insert("path", "/admin/structure/categories/add");

        return render_admin_template(&state, "admin/category-form.html", &context).await;
    }

    let input = CreateCategory {
        id: form.id.clone(),
        label: form.label.clone(),
        description: form.description.clone(),
        hierarchy: form.hierarchy.as_ref().and_then(|s| s.parse().ok()),
        weight: None,
    };

    match Category::create(state.db(), input).await {
        Ok(_) => {
            tracing::info!(id = %form.id, "category created");
            Redirect::to("/admin/structure/categories").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create category");
            render_error(&state, "Failed to create category.").await
        }
    }
}

/// Show edit category form.
///
/// GET /admin/structure/categories/{id}/edit
async fn edit_category_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found(&state).await;
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/structure/categories/{}/edit", category_id),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("category_id", &category_id);
    context.insert(
        "values",
        &serde_json::json!({
            "id": category.id,
            "label": category.label,
            "description": category.description,
            "hierarchy": category.hierarchy,
        }),
    );
    context.insert(
        "path",
        &format!("/admin/structure/categories/{}/edit", category_id),
    );

    render_admin_template(&state, "admin/category-form.html", &context).await
}

/// Handle edit category form submission.
///
/// POST /admin/structure/categories/{id}/edit
async fn edit_category_submit(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    Form(form): Form<CategoryFormData>,
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

    if !Category::exists(state.db(), &category_id)
        .await
        .unwrap_or(false)
    {
        return render_not_found(&state).await;
    }

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert(
            "action",
            &format!("/admin/structure/categories/{}/edit", category_id),
        );
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("category_id", &category_id);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "id": form.id,
                "label": form.label,
                "description": form.description,
                "hierarchy": form.hierarchy,
            }),
        );
        context.insert(
            "path",
            &format!("/admin/structure/categories/{}/edit", category_id),
        );

        return render_admin_template(&state, "admin/category-form.html", &context).await;
    }

    let input = UpdateCategory {
        label: Some(form.label.clone()),
        description: form.description.clone(),
        hierarchy: form.hierarchy.as_ref().and_then(|s| s.parse().ok()),
        weight: None,
    };

    match Category::update(state.db(), &category_id, input).await {
        Ok(_) => {
            tracing::info!(id = %category_id, "category updated");
            Redirect::to("/admin/structure/categories").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update category");
            render_error(&state, "Failed to update category.").await
        }
    }
}

/// Delete a category.
///
/// POST /admin/structure/categories/{id}/delete
async fn delete_category(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    match Category::delete(state.db(), &category_id).await {
        Ok(true) => {
            tracing::info!(id = %category_id, "category deleted");
            Redirect::to("/admin/structure/categories").into_response()
        }
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete category");
            render_error(&state, "Failed to delete category.").await
        }
    }
}

/// List tags in a category.
///
/// GET /admin/structure/categories/{id}/tags
async fn list_tags(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found(&state).await;
    };

    let tags = match Tag::list_by_category(state.db(), &category_id).await {
        Ok(tags) => tags,
        Err(e) => {
            tracing::error!(error = %e, "failed to list tags");
            return render_error(&state, "Failed to load tags.").await;
        }
    };

    let mut context = tera::Context::new();
    context.insert("category", &category);
    context.insert("tags", &tags);
    context.insert(
        "path",
        &format!("/admin/structure/categories/{}/tags", category_id),
    );

    render_admin_template(&state, "admin/tags.html", &context).await
}

/// Show add tag form.
///
/// GET /admin/structure/categories/{id}/tags/add
async fn add_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found(&state).await;
    };

    // Get existing tags for parent selector
    let tags = Tag::list_by_category(state.db(), &category_id)
        .await
        .unwrap_or_default();

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/structure/categories/{}/tags/add", category_id),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("category", &category);
    context.insert("existing_tags", &tags);
    context.insert("values", &serde_json::json!({}));
    context.insert(
        "path",
        &format!("/admin/structure/categories/{}/tags/add", category_id),
    );

    render_admin_template(&state, "admin/tag-form.html", &context).await
}

/// Handle add tag form submission.
///
/// POST /admin/structure/categories/{id}/tags/add
async fn add_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    Form(form): Form<TagFormData>,
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

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found(&state).await;
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let tags = Tag::list_by_category(state.db(), &category_id)
            .await
            .unwrap_or_default();
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert(
            "action",
            &format!("/admin/structure/categories/{}/tags/add", category_id),
        );
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("category", &category);
        context.insert("existing_tags", &tags);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "label": form.label,
                "description": form.description,
                "weight": form.weight,
                "parent_id": form.parent_id,
            }),
        );
        context.insert(
            "path",
            &format!("/admin/structure/categories/{}/tags/add", category_id),
        );

        return render_admin_template(&state, "admin/tag-form.html", &context).await;
    }

    let parent_ids = match &form.parent_id {
        Some(id) if !id.is_empty() => match uuid::Uuid::parse_str(id) {
            Ok(uuid) => Some(vec![uuid]),
            Err(_) => None,
        },
        _ => None,
    };

    let input = CreateTag {
        category_id: category_id.clone(),
        label: form.label.clone(),
        description: form.description.clone(),
        weight: form.weight.as_ref().and_then(|s| s.parse().ok()),
        parent_ids,
    };

    match Tag::create(state.db(), input).await {
        Ok(_) => {
            tracing::info!(category = %category_id, label = %form.label, "tag created");
            Redirect::to(&format!("/admin/structure/categories/{}/tags", category_id))
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create tag");
            render_error(&state, "Failed to create tag.").await
        }
    }
}

/// Show edit tag form.
///
/// GET /admin/structure/tags/{id}/edit
async fn edit_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(tag) = Tag::find_by_id(state.db(), tag_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    let Some(category) = Category::find_by_id(state.db(), &tag.category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_error(&state, "Category not found.").await;
    };

    // Get existing tags for parent selector (excluding self)
    let tags: Vec<_> = Tag::list_by_category(state.db(), &tag.category_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.id != tag_id)
        .collect();

    // Get current parents
    let parents = Tag::get_parents(state.db(), tag_id)
        .await
        .unwrap_or_default();
    let current_parent_id = parents.first().map(|p| p.id.to_string());

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/structure/tags/{}/edit", tag_id));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("tag_id", &tag_id.to_string());
    context.insert("category", &category);
    context.insert("existing_tags", &tags);
    context.insert(
        "values",
        &serde_json::json!({
            "label": tag.label,
            "description": tag.description,
            "weight": tag.weight,
            "parent_id": current_parent_id,
        }),
    );
    context.insert("path", &format!("/admin/structure/tags/{}/edit", tag_id));

    render_admin_template(&state, "admin/tag-form.html", &context).await
}

/// Handle edit tag form submission.
///
/// POST /admin/structure/tags/{id}/edit
async fn edit_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
    Form(form): Form<TagFormData>,
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

    let Some(tag) = Tag::find_by_id(state.db(), tag_id).await.ok().flatten() else {
        return render_not_found(&state).await;
    };

    let Some(category) = Category::find_by_id(state.db(), &tag.category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_error(&state, "Category not found.").await;
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let tags: Vec<_> = Tag::list_by_category(state.db(), &tag.category_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.id != tag_id)
            .collect();

        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/structure/tags/{}/edit", tag_id));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("tag_id", &tag_id.to_string());
        context.insert("category", &category);
        context.insert("existing_tags", &tags);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "label": form.label,
                "description": form.description,
                "weight": form.weight,
                "parent_id": form.parent_id,
            }),
        );
        context.insert("path", &format!("/admin/structure/tags/{}/edit", tag_id));

        return render_admin_template(&state, "admin/tag-form.html", &context).await;
    }

    // Update tag
    let input = UpdateTag {
        label: Some(form.label.clone()),
        description: form.description.clone(),
        weight: form.weight.as_ref().and_then(|s| s.parse().ok()),
    };

    if let Err(e) = Tag::update(state.db(), tag_id, input).await {
        tracing::error!(error = %e, "failed to update tag");
        return render_error(&state, "Failed to update tag.").await;
    }

    // Update parent if hierarchy is enabled
    if category.hierarchy > 0 {
        let parent_ids: Vec<uuid::Uuid> = match &form.parent_id {
            Some(id) if !id.is_empty() => match uuid::Uuid::parse_str(id) {
                Ok(uuid) => vec![uuid],
                Err(_) => vec![],
            },
            _ => vec![],
        };

        if let Err(e) = Tag::set_parents(state.db(), tag_id, &parent_ids).await {
            tracing::error!(error = %e, "failed to update tag parents");
        }
    }

    tracing::info!(tag_id = %tag_id, "tag updated");
    Redirect::to(&format!(
        "/admin/structure/categories/{}/tags",
        tag.category_id
    ))
    .into_response()
}

/// Delete a tag.
///
/// POST /admin/structure/tags/{id}/delete
async fn delete_tag(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Get category ID for redirect
    let category_id = Tag::find_by_id(state.db(), tag_id)
        .await
        .ok()
        .flatten()
        .map(|t| t.category_id);

    match Tag::delete(state.db(), tag_id).await {
        Ok(true) => {
            tracing::info!(tag_id = %tag_id, "tag deleted");
            let redirect_url = category_id
                .map(|id| format!("/admin/structure/categories/{}/tags", id))
                .unwrap_or_else(|| "/admin/structure/categories".to_string());
            Redirect::to(&redirect_url).into_response()
        }
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete tag");
            render_error(&state, "Failed to delete tag.").await
        }
    }
}

// =============================================================================
// URL Alias Management
// =============================================================================

/// URL alias form data.
#[derive(Debug, Deserialize)]
pub struct UrlAliasFormData {
    #[serde(rename = "_token")]
    pub token: String,
    pub source: String,
    pub alias: String,
    pub language: Option<String>,
}

/// Alias display struct for templates.
#[derive(Debug, Serialize)]
struct AliasDisplay {
    id: uuid::Uuid,
    source: String,
    alias: String,
    language: String,
    created_display: String,
}

/// List all URL aliases.
///
/// GET /admin/structure/aliases
async fn list_aliases(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: i64 = 50;
    let offset = (page - 1) * per_page;

    let aliases = match UrlAlias::list_all(state.db(), per_page, offset).await {
        Ok(aliases) => aliases,
        Err(e) => {
            tracing::error!(error = %e, "failed to list url aliases");
            return render_error(&state, "Failed to load URL aliases.").await;
        }
    };

    let total = UrlAlias::count_all(state.db()).await.unwrap_or(0);
    let total_pages = (total as f64 / per_page as f64).ceil() as i64;

    // Convert to display structs with formatted dates
    let aliases_display: Vec<AliasDisplay> = aliases
        .into_iter()
        .map(|a| AliasDisplay {
            id: a.id,
            source: a.source,
            alias: a.alias,
            language: a.language,
            created_display: chrono::DateTime::from_timestamp(a.created, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "Unknown".to_string()),
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert("aliases", &aliases_display);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("total_pages", &total_pages);
    context.insert("path", "/admin/structure/aliases");

    render_admin_template(&state, "admin/aliases.html", &context).await
}

/// Add URL alias form.
///
/// GET /admin/structure/aliases/add
async fn add_alias_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("action", "/admin/structure/aliases/add");
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/structure/aliases/add");

    render_admin_template(&state, "admin/alias-form.html", &context).await
}

/// Add URL alias submit.
///
/// POST /admin/structure/aliases/add
async fn add_alias_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<UrlAliasFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error(&state, "Invalid or expired form token. Please try again.").await;
    }

    // Normalize paths
    let source = if form.source.starts_with('/') {
        form.source.clone()
    } else {
        format!("/{}", form.source)
    };

    let alias = if form.alias.starts_with('/') {
        form.alias.clone()
    } else {
        format!("/{}", form.alias)
    };

    let input = CreateUrlAlias {
        source,
        alias,
        language: form.language,
        stage_id: Some("live".to_string()),
    };

    match UrlAlias::create(state.db(), input).await {
        Ok(_) => Redirect::to("/admin/structure/aliases").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to create url alias");
            render_error(
                &state,
                "Failed to create URL alias. The alias may already exist.",
            )
            .await
        }
    }
}

/// Edit URL alias form.
///
/// GET /admin/structure/aliases/{id}/edit
async fn edit_alias_form(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let alias = match UrlAlias::find_by_id(state.db(), alias_id).await {
        Ok(Some(alias)) => alias,
        Ok(None) => return render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to load url alias");
            return render_error(&state, "Failed to load URL alias.").await;
        }
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert(
        "action",
        &format!("/admin/structure/aliases/{}/edit", alias_id),
    );
    context.insert("editing", &true);
    context.insert(
        "values",
        &serde_json::json!({
            "source": alias.source,
            "alias": alias.alias,
            "language": alias.language,
        }),
    );
    context.insert(
        "path",
        &format!("/admin/structure/aliases/{}/edit", alias_id),
    );

    render_admin_template(&state, "admin/alias-form.html", &context).await
}

/// Edit URL alias submit.
///
/// POST /admin/structure/aliases/{id}/edit
async fn edit_alias_submit(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
    Form(form): Form<UrlAliasFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error(&state, "Invalid or expired form token. Please try again.").await;
    }

    // Normalize paths
    let source = if form.source.starts_with('/') {
        form.source.clone()
    } else {
        format!("/{}", form.source)
    };

    let alias = if form.alias.starts_with('/') {
        form.alias.clone()
    } else {
        format!("/{}", form.alias)
    };

    let input = UpdateUrlAlias {
        source: Some(source),
        alias: Some(alias),
        language: form.language,
        stage_id: None,
    };

    match UrlAlias::update(state.db(), alias_id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/structure/aliases").into_response(),
        Ok(None) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to update url alias");
            render_error(&state, "Failed to update URL alias.").await
        }
    }
}

/// Delete URL alias.
///
/// POST /admin/structure/aliases/{id}/delete
async fn delete_alias(
    State(state): State<AppState>,
    session: Session,
    Path(alias_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    match UrlAlias::delete(state.db(), alias_id).await {
        Ok(true) => {
            tracing::info!(alias_id = %alias_id, "url alias deleted");
            Redirect::to("/admin/structure/aliases").into_response()
        }
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete url alias");
            render_error(&state, "Failed to delete URL alias.").await
        }
    }
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
    if let Err(redirect) = require_auth(&state, &session).await {
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
            return render_error(&state, "Failed to load files.").await;
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

    let mut context = tera::Context::new();
    context.insert("files", &files);
    context.insert("owners", &owners);
    context.insert("status_filter", &status_filter.map(|s| s as i16));
    context.insert("path", "/admin/content/files");

    render_admin_template(&state, "admin/files.html", &context).await
}

/// Show file details.
///
/// GET /admin/content/files/{id}
async fn file_details(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(file) = state.files().get(file_id).await.ok().flatten() else {
        return render_not_found(&state).await;
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

    render_admin_template(&state, "admin/file-details.html", &context).await
}

/// Delete a file.
///
/// POST /admin/content/files/{id}/delete
async fn delete_file(
    State(state): State<AppState>,
    session: Session,
    Path(file_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    match state.files().delete(file_id).await {
        Ok(true) => {
            tracing::info!(file_id = %file_id, "file deleted");
            Redirect::to("/admin/content/files").into_response()
        }
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete file");
            render_error(&state, "Failed to delete file.").await
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
async fn handle_ajax_add_field(state: &AppState, request: &AjaxRequest) -> Response {
    use crate::form::AjaxResponse;

    // Load form state to get the content type name
    let form_state = match state.forms().load_state(&request.form_build_id).await {
        Ok(Some(fs)) => fs,
        Ok(None) => {
            return Json(
                AjaxResponse::new().alert("Form session expired. Please reload the page."),
            )
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
            .invoke("Trovato.resetAddFieldForm", serde_json::json!({}))
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

// =============================================================================
// Comment Moderation
// =============================================================================

/// Query params for comment list.
#[derive(Debug, Deserialize)]
pub struct CommentListQuery {
    pub status: Option<i16>,
    pub page: Option<i64>,
}

/// Form data for editing a comment.
#[derive(Debug, Deserialize)]
pub struct EditCommentForm {
    pub body: String,
    pub status: i16,
}

/// List all comments for moderation.
///
/// GET /admin/content/comments
async fn list_comments(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<CommentListQuery>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
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
    context.insert("path", "/admin/content/comments");

    render_admin_template(&state, "admin/comments.html", &context).await
}

/// Edit a comment form.
///
/// GET /admin/content/comments/{id}/edit
async fn edit_comment_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let comment = match Comment::find_by_id(state.db(), id).await {
        Ok(Some(c)) => c,
        Ok(None) => return render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to load comment");
            return render_error(&state, "Failed to load comment").await;
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

    let mut context = tera::Context::new();
    context.insert("comment", &comment);
    context.insert("author_name", &author_name);
    context.insert("item_title", &item_title);
    context.insert("path", "/admin/content/comments");

    render_admin_template(&state, "admin/comment-form.html", &context).await
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
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let input = UpdateComment {
        body: Some(form.body),
        body_format: None,
        status: Some(form.status),
    };

    match Comment::update(state.db(), id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/content/comments").into_response(),
        Ok(None) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to update comment");
            render_error(&state, "Failed to update comment").await
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
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let input = UpdateComment {
        body: None,
        body_format: None,
        status: Some(1),
    };

    match Comment::update(state.db(), id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/content/comments").into_response(),
        Ok(None) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to approve comment");
            render_error(&state, "Failed to approve comment").await
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
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let input = UpdateComment {
        body: None,
        body_format: None,
        status: Some(0),
    };

    match Comment::update(state.db(), id, input).await {
        Ok(Some(_)) => Redirect::to("/admin/content/comments").into_response(),
        Ok(None) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to unpublish comment");
            render_error(&state, "Failed to unpublish comment").await
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
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    match Comment::delete(state.db(), id).await {
        Ok(true) => Redirect::to("/admin/content/comments").into_response(),
        Ok(false) => render_not_found(&state).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to delete comment");
            render_error(&state, "Failed to delete comment").await
        }
    }
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
async fn render_error(_state: &AppState, message: &str) -> Response {
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
async fn render_not_found(_state: &AppState) -> Response {
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
        // User management
        .route("/admin/people", get(list_users))
        .route("/admin/people/add", get(add_user_form))
        .route("/admin/people/add", post(add_user_submit))
        .route("/admin/people/{id}/edit", get(edit_user_form))
        .route("/admin/people/{id}/edit", post(edit_user_submit))
        .route("/admin/people/{id}/delete", post(delete_user))
        // Role management
        .route("/admin/people/roles", get(list_roles))
        .route("/admin/people/roles/add", get(add_role_form))
        .route("/admin/people/roles/add", post(add_role_submit))
        .route("/admin/people/roles/{id}/edit", get(edit_role_form))
        .route("/admin/people/roles/{id}/edit", post(edit_role_submit))
        .route("/admin/people/roles/{id}/delete", post(delete_role))
        // Permission management
        .route("/admin/people/permissions", get(permissions_matrix))
        .route("/admin/people/permissions", post(save_permissions))
        // Content management
        .route("/admin/content", get(list_content))
        .route("/admin/content/add", get(select_content_type))
        .route("/admin/content/add/{type}", get(add_content_form))
        .route("/admin/content/add/{type}", post(add_content_submit))
        .route("/admin/content/{id}/edit", get(edit_content_form))
        .route("/admin/content/{id}/edit", post(edit_content_submit))
        .route("/admin/content/{id}/delete", post(delete_content))
        // File management
        .route("/admin/content/files", get(list_files))
        .route("/admin/content/files/{id}", get(file_details))
        .route("/admin/content/files/{id}/delete", post(delete_file))
        // Comment moderation
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
        // Content type management
        .route("/admin/structure/types", get(list_content_types))
        .route("/admin/structure/types/add", get(add_content_type_form))
        .route("/admin/structure/types/add", post(add_content_type_submit))
        .route(
            "/admin/structure/types/{type}/edit",
            get(edit_content_type_form),
        )
        .route(
            "/admin/structure/types/{type}/edit",
            post(edit_content_type_submit),
        )
        .route("/admin/structure/types/{type}/fields", get(manage_fields))
        .route("/admin/structure/types/{type}/fields/add", post(add_field))
        // Search configuration
        .route(
            "/admin/structure/types/{type}/search",
            get(manage_search_config),
        )
        .route(
            "/admin/structure/types/{type}/search/add",
            post(add_search_config),
        )
        .route(
            "/admin/structure/types/{type}/search/{field}/delete",
            post(remove_search_config),
        )
        .route(
            "/admin/structure/types/{type}/search/reindex",
            post(reindex_content_type),
        )
        // Category management
        .route("/admin/structure/categories", get(list_categories))
        .route("/admin/structure/categories/add", get(add_category_form))
        .route("/admin/structure/categories/add", post(add_category_submit))
        .route(
            "/admin/structure/categories/{id}/edit",
            get(edit_category_form),
        )
        .route(
            "/admin/structure/categories/{id}/edit",
            post(edit_category_submit),
        )
        .route(
            "/admin/structure/categories/{id}/delete",
            post(delete_category),
        )
        .route("/admin/structure/categories/{id}/tags", get(list_tags))
        .route(
            "/admin/structure/categories/{id}/tags/add",
            get(add_tag_form),
        )
        .route(
            "/admin/structure/categories/{id}/tags/add",
            post(add_tag_submit),
        )
        // Tag management
        .route("/admin/structure/tags/{id}/edit", get(edit_tag_form))
        .route("/admin/structure/tags/{id}/edit", post(edit_tag_submit))
        .route("/admin/structure/tags/{id}/delete", post(delete_tag))
        // URL Alias management
        .route("/admin/structure/aliases", get(list_aliases))
        .route("/admin/structure/aliases/add", get(add_alias_form))
        .route("/admin/structure/aliases/add", post(add_alias_submit))
        .route("/admin/structure/aliases/{id}/edit", get(edit_alias_form))
        .route(
            "/admin/structure/aliases/{id}/edit",
            post(edit_alias_submit),
        )
        .route("/admin/structure/aliases/{id}/delete", post(delete_alias))
        // AJAX endpoint
        .route("/system/ajax", post(ajax_callback))
}
