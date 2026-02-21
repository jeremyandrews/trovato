//! Admin routes for user, role, and permission management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::role::well_known::{ANONYMOUS_ROLE_ID, AUTHENTICATED_ROLE_ID};
use crate::models::user::ANONYMOUS_USER_ID;
use crate::models::{CreateUser, Role, UpdateUser, User};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, build_local_tasks, render_admin_template, render_error, render_not_found,
    render_server_error, require_admin, require_csrf,
};

/// Permissions available for assignment to roles.
///
/// Used by both `permissions_matrix` (display) and `save_permissions` (processing).
const AVAILABLE_PERMISSIONS: &[&str] = &[
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
    "use filtered_html",
    "use full_html",
];

/// User form data.
#[derive(Debug, Deserialize)]
struct UserFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    name: String,
    mail: String,
    password: Option<String>,
    is_admin: Option<String>,
    status: Option<String>,
}

/// Role form data.
#[derive(Debug, Deserialize)]
struct RoleFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    name: String,
}

/// Permission form data (for permission matrix).
#[derive(Debug, Deserialize)]
struct PermissionFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    #[serde(flatten)]
    permissions: std::collections::HashMap<String, String>,
}

// =============================================================================
// User Management
// =============================================================================

/// List all users.
///
/// GET /admin/people
async fn list_users(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let users = match User::list(state.db()).await {
        Ok(users) => users,
        Err(e) => {
            tracing::error!(error = %e, "failed to list users");
            return render_server_error("Failed to load users.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("users", &users);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/people");

    render_admin_template(&state, "admin/users.html", context).await
}

/// Show add user form.
///
/// GET /admin/people/add
async fn add_user_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
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

    render_admin_template(&state, "admin/user-form.html", context).await
}

/// Handle add user form submission.
///
/// POST /admin/people/add
async fn add_user_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<UserFormData>,
) -> Response {
    let current_user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
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

        return render_admin_template(&state, "admin/user-form.html", context).await;
    }

    // Create the user
    let input = CreateUser {
        name: form.name.clone(),
        password: password.to_string(),
        mail: form.mail.clone(),
        is_admin: form.is_admin.is_some(),
    };

    match User::create(state.db(), input).await {
        Ok(user) => {
            // Dispatch tap_user_register
            let tap_input = serde_json::json!({ "user_id": user.id.to_string() });
            let tap_state = crate::tap::RequestState::without_services(
                crate::tap::UserContext::authenticated(current_user.id, vec![]),
            );
            state
                .tap_dispatcher()
                .dispatch("tap_user_register", &tap_input.to_string(), tap_state)
                .await;

            tracing::info!(name = %form.name, "user created");
            Redirect::to("/admin/people").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create user");
            render_server_error("Failed to create user.")
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(user) = User::find_by_id(state.db(), user_id).await.ok().flatten() else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/people/{user_id}/edit"));
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
    context.insert("path", &format!("/admin/people/{user_id}/edit"));

    // Local task tabs for user edit pages (hardcoded + plugin-registered)
    let current_path = format!("/admin/people/{user_id}/edit");
    context.insert(
        "local_tasks",
        &build_local_tasks(
            &state,
            "/admin/people/:id",
            &current_path,
            Some(&user_id.to_string()),
            vec![serde_json::json!({"title": "Edit", "path": &current_path, "active": true})],
        ),
    );

    render_admin_template(&state, "admin/user-form.html", context).await
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
    let current_user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(existing_user) = User::find_by_id(state.db(), user_id).await.ok().flatten() else {
        return render_not_found();
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
    if form.name != existing_user.name
        && let Ok(Some(_)) = User::find_by_name(state.db(), &form.name).await
    {
        errors.push(format!("Username '{}' is already taken.", form.name));
    }

    // Check if new email is taken by someone else
    if form.mail != existing_user.mail
        && let Ok(Some(_)) = User::find_by_mail(state.db(), &form.mail).await
    {
        errors.push(format!("Email '{}' is already in use.", form.mail));
    }

    // Validate password if provided
    if let Some(ref password) = form.password
        && !password.is_empty()
        && password.len() < 8
    {
        errors.push("Password must be at least 8 characters.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/people/{user_id}/edit"));
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
        context.insert("path", &format!("/admin/people/{user_id}/edit"));
        context.insert(
            "local_tasks",
            &serde_json::json!([
                {"title": "Edit", "path": format!("/admin/people/{user_id}/edit"), "active": true},
            ]),
        );

        return render_admin_template(&state, "admin/user-form.html", context).await;
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
            if let Some(ref password) = form.password
                && !password.is_empty()
                && let Err(e) = User::update_password(state.db(), user_id, password).await
            {
                tracing::error!(error = %e, "failed to update user password");
                return render_server_error("Failed to update password.");
            }

            // Dispatch tap_user_update
            let tap_input = serde_json::json!({ "user_id": user_id.to_string() });
            let tap_state = crate::tap::RequestState::without_services(
                crate::tap::UserContext::authenticated(current_user.id, vec![]),
            );
            state
                .tap_dispatcher()
                .dispatch("tap_user_update", &tap_input.to_string(), tap_state)
                .await;

            tracing::info!(user_id = %user_id, "user updated");
            Redirect::to("/admin/people").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update user");
            render_server_error("Failed to update user.")
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
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let current_user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Prevent deleting anonymous user
    if user_id == ANONYMOUS_USER_ID {
        return render_error("Cannot delete the anonymous user.");
    }

    // Prevent deleting yourself
    if user_id == current_user.id {
        return render_error("Cannot delete your own account.");
    }

    match User::delete(state.db(), user_id).await {
        Ok(true) => {
            // Dispatch tap_user_delete
            let tap_input = serde_json::json!({ "user_id": user_id.to_string() });
            let tap_state = crate::tap::RequestState::without_services(
                crate::tap::UserContext::authenticated(current_user.id, vec![]),
            );
            state
                .tap_dispatcher()
                .dispatch("tap_user_delete", &tap_input.to_string(), tap_state)
                .await;

            tracing::info!(user_id = %user_id, "user deleted");
            Redirect::to("/admin/people").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete user");
            render_server_error("Failed to delete user.")
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let roles = match Role::list(state.db()).await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_server_error("Failed to load roles.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("roles", &roles);
    context.insert("anonymous_role_id", &ANONYMOUS_ROLE_ID.to_string());
    context.insert("authenticated_role_id", &AUTHENTICATED_ROLE_ID.to_string());
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/people/roles");

    render_admin_template(&state, "admin/roles.html", context).await
}

/// Show add role form.
///
/// GET /admin/people/roles/add
async fn add_role_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
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

    render_admin_template(&state, "admin/role-form.html", context).await
}

/// Handle add role form submission.
///
/// POST /admin/people/roles/add
async fn add_role_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RoleFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
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

        return render_admin_template(&state, "admin/role-form.html", context).await;
    }

    match Role::create(state.db(), &form.name).await {
        Ok(_) => {
            tracing::info!(name = %form.name, "role created");
            Redirect::to("/admin/people/roles").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create role");
            render_server_error("Failed to create role.")
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(role) = Role::find_by_id(state.db(), role_id).await.ok().flatten() else {
        return render_not_found();
    };

    let permissions = Role::get_permissions(state.db(), role_id)
        .await
        .unwrap_or_default();

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/people/roles/{role_id}/edit"));
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
    context.insert("path", &format!("/admin/people/roles/{role_id}/edit"));

    render_admin_template(&state, "admin/role-form.html", context).await
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(existing_role) = Role::find_by_id(state.db(), role_id).await.ok().flatten() else {
        return render_not_found();
    };

    // Validate
    let mut errors = Vec::new();

    if form.name.trim().is_empty() {
        errors.push("Role name is required.".to_string());
    }

    // Check if new name is taken by someone else
    if form.name != existing_role.name
        && let Ok(Some(_)) = Role::find_by_name(state.db(), &form.name).await
    {
        errors.push(format!("A role named '{}' already exists.", form.name));
    }

    if !errors.is_empty() {
        let permissions = Role::get_permissions(state.db(), role_id)
            .await
            .unwrap_or_default();
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/people/roles/{role_id}/edit"));
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
        context.insert("path", &format!("/admin/people/roles/{role_id}/edit"));

        return render_admin_template(&state, "admin/role-form.html", context).await;
    }

    match Role::update(state.db(), role_id, &form.name).await {
        Ok(_) => {
            tracing::info!(role_id = %role_id, "role updated");
            Redirect::to("/admin/people/roles").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update role");
            render_server_error("Failed to update role.")
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
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Prevent deleting built-in roles
    if role_id == ANONYMOUS_ROLE_ID || role_id == AUTHENTICATED_ROLE_ID {
        return render_error("Cannot delete built-in roles.");
    }

    match Role::delete(state.db(), role_id).await {
        Ok(true) => {
            tracing::info!(role_id = %role_id, "role deleted");
            Redirect::to("/admin/people/roles").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete role");
            render_server_error("Failed to delete role.")
        }
    }
}

/// Show permission matrix.
///
/// GET /admin/people/permissions
async fn permissions_matrix(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let roles = match Role::list(state.db()).await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_error("Failed to load roles.");
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

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("roles", &roles);
    context.insert("role_permissions", &role_permissions);
    context.insert("available_permissions", &AVAILABLE_PERMISSIONS);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("path", "/admin/people/permissions");

    render_admin_template(&state, "admin/permissions.html", context).await
}

/// Save permission matrix.
///
/// POST /admin/people/permissions
async fn save_permissions(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<PermissionFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let roles = match Role::list(state.db()).await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_server_error("Failed to load roles.");
        }
    };

    // Process form data - permissions are submitted as "perm_{role_id}_{permission}"
    for role in &roles {
        let current_perms = Role::get_permissions(state.db(), role.id)
            .await
            .unwrap_or_default();

        for permission in AVAILABLE_PERMISSIONS {
            let key = format!("perm_{}_{}", role.id, permission.replace(' ', "_"));
            let should_have = form.permissions.contains_key(&key);
            let has_now = current_perms.contains(&permission.to_string());

            if should_have && !has_now {
                if let Err(e) = Role::add_permission(state.db(), role.id, permission).await {
                    tracing::error!(error = %e, role_id = %role.id, permission = %permission, "failed to add permission");
                }
            } else if !should_have
                && has_now
                && let Err(e) = Role::remove_permission(state.db(), role.id, permission).await
            {
                tracing::error!(error = %e, role_id = %role.id, permission = %permission, "failed to remove permission");
            }
        }
    }

    tracing::info!("permissions updated");
    Redirect::to("/admin/people/permissions").into_response()
}

/// Build the router for admin user, role, and permission routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/people", get(list_users))
        .route(
            "/admin/people/add",
            get(add_user_form).post(add_user_submit),
        )
        .route(
            "/admin/people/{id}/edit",
            get(edit_user_form).post(edit_user_submit),
        )
        .route("/admin/people/{id}/delete", post(delete_user))
        .route("/admin/people/roles", get(list_roles))
        .route(
            "/admin/people/roles/add",
            get(add_role_form).post(add_role_submit),
        )
        .route(
            "/admin/people/roles/{id}/edit",
            get(edit_role_form).post(edit_role_submit),
        )
        .route("/admin/people/roles/{id}/delete", post(delete_role))
        .route(
            "/admin/people/permissions",
            get(permissions_matrix).post(save_permissions),
        )
}
