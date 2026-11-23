//! Admin routes for content type management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::content::{AddFieldError, CreateContentTypeError};
use crate::form::csrf::generate_csrf_token;
use crate::search::SearchSwitches;
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, MACHINE_NAME_ERROR, html_escape, is_valid_machine_name, render_admin_template,
    render_error, render_not_found, render_server_error, require_csrf, require_permission,
};

// =============================================================================
// Form data
// =============================================================================

/// Content type form data.
#[derive(Debug, Deserialize)]
struct ContentTypeFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    label: String,
    machine_name: String,
    description: Option<String>,
    title_label: Option<String>,
    published_default: Option<String>,
    revision_default: Option<String>,
    promote_default: Option<String>,
    sticky_default: Option<String>,
}

/// The per-type defaults the content type form edits, by settings key. Each
/// is a checkbox, stored as a boolean in the type's `settings`.
const DEFAULT_SWITCHES: &[&str] = &[
    "published_default",
    "revision_default",
    "promote_default",
    "sticky_default",
];

impl ContentTypeFormData {
    /// Whether the checkbox for settings key `key` was checked.
    fn switch(&self, key: &str) -> bool {
        match key {
            "published_default" => self.published_default.is_some(),
            "revision_default" => self.revision_default.is_some(),
            "promote_default" => self.promote_default.is_some(),
            "sticky_default" => self.sticky_default.is_some(),
            _ => false,
        }
    }

    /// The settings this submission stores, merged into what the type holds.
    fn settings(&self) -> serde_json::Value {
        let mut settings = serde_json::Map::new();
        settings.insert(
            "title_label".to_string(),
            serde_json::Value::from(
                self.title_label
                    .clone()
                    .unwrap_or_else(|| "Title".to_string()),
            ),
        );
        for key in DEFAULT_SWITCHES {
            settings.insert(
                (*key).to_string(),
                serde_json::Value::Bool(self.switch(key)),
            );
        }
        serde_json::Value::Object(settings)
    }

    /// The values to show this submission again with.
    fn values(&self) -> serde_json::Value {
        let mut values = serde_json::json!({
            "label": self.label,
            "machine_name": self.machine_name,
            "description": self.description,
            "title_label": self.title_label,
        });
        for key in DEFAULT_SWITCHES {
            values[*key] = serde_json::Value::Bool(self.switch(key));
        }
        values
    }
}

/// Field form data.
#[derive(Debug, Deserialize)]
struct FieldFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    label: String,
    name: String,
    field_type: String,
}

/// Search field configuration form data.
#[derive(Debug, Deserialize)]
struct SearchConfigFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    field_name: String,
    weight: String,
}

// =============================================================================
// Content Type Management
// =============================================================================

/// List all content types.
///
/// GET /admin/structure/types
async fn list_content_types(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/structure/types");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-types.html",
        context,
    )
    .await
}

/// Show add content type form.
///
/// GET /admin/structure/types/add
async fn add_content_type_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/types/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/structure/types/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-type-form.html",
        context,
    )
    .await
}

/// Handle add content type form submission.
///
/// POST /admin/structure/types/add
async fn add_content_type_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ContentTypeFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if form.machine_name.trim().is_empty() {
        errors.push("Machine name is required.".to_string());
    } else if !is_valid_machine_name(&form.machine_name) {
        errors.push(MACHINE_NAME_ERROR.to_string());
    }

    // Check if machine name already exists. The cache answers the common
    // case; `create` refuses a name only the database knows.
    if state.content_types().get(&form.machine_name).is_some() {
        errors.push(
            CreateContentTypeError::AlreadyExists {
                machine_name: form.machine_name.clone(),
            }
            .to_string(),
        );
    }

    if !errors.is_empty() {
        return render_add_form_with_errors(&state, &session, &form, &errors).await;
    }

    // Create the content type
    let settings = form.settings();

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
        Ok(()) => {
            tracing::info!(machine_name = %form.machine_name, "content type created");
            Redirect::to("/admin/structure/types").into_response()
        }
        Err(e @ CreateContentTypeError::AlreadyExists { .. }) => {
            render_add_form_with_errors(&state, &session, &form, &[e.to_string()]).await
        }
        Err(CreateContentTypeError::Other(e)) => {
            tracing::error!(error = %e, "failed to create content type");
            render_server_error("Failed to create content type.")
        }
    }
}

/// Render the add content type form again, with `errors` and the submitted
/// values.
async fn render_add_form_with_errors(
    state: &AppState,
    session: &Session,
    form: &ContentTypeFormData,
    errors: &[String],
) -> Response {
    let csrf_token = generate_csrf_token(session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/types/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("errors", &errors);
    context.insert("values", &form.values());
    context.insert("path", "/admin/structure/types/add");

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/content-type-form.html",
        context,
    )
    .await
}

/// Show edit content type form.
///
/// GET /admin/structure/types/{type}/edit
async fn edit_content_type_form(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/structure/types/{type_name}/edit"),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    // Load the database model to access settings (not available on ContentTypeDefinition)
    let db_type = crate::models::ItemType::find_by_type(state.db(), &type_name)
        .await
        .ok()
        .flatten();
    let title_label = db_type
        .as_ref()
        .and_then(|it| it.title_label.as_deref())
        .unwrap_or("Title");
    let mut values = serde_json::json!({
        "label": content_type.label,
        "machine_name": content_type.machine_name,
        "description": content_type.description,
        "title_label": title_label,
    });
    for key in DEFAULT_SWITCHES {
        values[*key] = serde_json::Value::Bool(
            db_type
                .as_ref()
                .and_then(|it| it.settings.get(*key))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        );
    }
    context.insert("values", &values);
    context.insert("path", &format!("/admin/structure/types/{type_name}/edit"));

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-type-form.html",
        context,
    )
    .await
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert(
            "action",
            &format!("/admin/structure/types/{type_name}/edit"),
        );
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("errors", &errors);
        context.insert("values", &form.values());
        context.insert("path", &format!("/admin/structure/types/{type_name}/edit"));

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/content-type-form.html",
            context,
        )
        .await;
    }

    // Update the content type
    let settings = form.settings();

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
            render_server_error("Failed to update content type.")
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

    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    // Save initial form state for AJAX callbacks
    let form_state = FormState::new(format!("manage_fields_{type_name}"), form_build_id.clone());

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
        &format!("/admin/structure/types/{type_name}/fields"),
    );

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/field-list.html",
        context,
    )
    .await
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Label is required.".to_string());
    }

    if form.name.trim().is_empty() {
        errors.push("Machine name is required.".to_string());
    } else if !is_valid_machine_name(&form.name) {
        errors.push(MACHINE_NAME_ERROR.to_string());
    }

    if form.field_type.is_empty() {
        errors.push("Field type is required.".to_string());
    }

    if !errors.is_empty() {
        return rerender_field_list(&state, &session, &type_name, &form, errors).await;
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
            Redirect::to(&format!("/admin/structure/types/{type_name}/fields")).into_response()
        }
        // The two the person at the screen can fix are shown to them. The
        // registry owns the wording, so it is not written out again here.
        Err(
            e @ (AddFieldError::DuplicateFieldName { .. } | AddFieldError::UnknownFieldType { .. }),
        ) => rerender_field_list(&state, &session, &type_name, &form, vec![e.to_string()]).await,
        Err(e) => {
            tracing::error!(error = %e, "failed to add field");
            render_server_error("Failed to add field.")
        }
    }
}

/// Re-render the manage-fields page with `errors` shown and the submitted
/// values kept, so a refused add does not make the person type it again.
async fn rerender_field_list(
    state: &AppState,
    session: &Session,
    type_name: &str,
    form: &FieldFormData,
    errors: Vec<String>,
) -> Response {
    let Some(content_type) = state.content_types().get(type_name) else {
        return render_not_found();
    };
    let csrf_token = generate_csrf_token(session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    // Save form state for AJAX callbacks
    let form_state =
        crate::form::FormState::new(format!("manage_fields_{type_name}"), form_build_id.clone());
    if let Err(e) = state.forms().save_state(&form_build_id, &form_state).await {
        tracing::warn!(error = %e, "failed to save form state");
    }

    let mut context = tera::Context::new();
    context.insert("content_type", &content_type);
    context.insert("fields", &content_type.fields);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("errors", &errors);
    context.insert(
        "values",
        &serde_json::json!({
            "label": form.label,
            "name": form.name,
            "field_type": form.field_type,
        }),
    );
    context.insert(
        "path",
        &format!("/admin/structure/types/{type_name}/fields"),
    );

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/field-list.html",
        context,
    )
    .await
}

// =============================================================================
// Field Edit / Delete
// =============================================================================

/// Form data for field editing.
#[derive(Debug, Deserialize)]
struct FieldEditFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    _form_build_id: String,
    label: String,
    required: Option<String>,
    cardinality: Option<i32>,
}

/// Show field edit form.
///
/// GET /admin/structure/types/{type}/fields/{field}/edit
async fn edit_field_form(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let Some(field) = content_type
        .fields
        .iter()
        .find(|f| f.field_name == field_name)
    else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("type_name", &type_name);
    context.insert("field", field);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert(
        "path",
        &format!("/admin/structure/types/{type_name}/fields/{field_name}/edit"),
    );

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/field-edit.html",
        context,
    )
    .await
}

/// Handle field edit form submission.
///
/// POST /admin/structure/types/{type}/fields/{field}/edit
async fn edit_field_submit(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
    Form(form): Form<FieldEditFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let Some(field) = content_type
        .fields
        .iter()
        .find(|f| f.field_name == field_name)
    else {
        return render_not_found();
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Label is required.".to_string());
    }

    let cardinality = form.cardinality.unwrap_or(1);
    if cardinality == 0 || cardinality < -1 {
        errors.push("Number of values must be 1 or more, or -1 for unlimited.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        // Build a modified field with form values for re-rendering
        let mut edited_field = field.clone();
        edited_field.label = form.label;
        edited_field.required = form.required.is_some();
        edited_field.cardinality = cardinality;

        let mut context = tera::Context::new();
        context.insert("type_name", &type_name);
        context.insert("field", &edited_field);
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("errors", &errors);
        context.insert(
            "path",
            &format!("/admin/structure/types/{type_name}/fields/{field_name}/edit"),
        );

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/field-edit.html",
            context,
        )
        .await;
    }

    let required = form.required.is_some();

    match state
        .content_types()
        .update_field(&type_name, &field_name, &form.label, required, cardinality)
        .await
    {
        Ok(_) => {
            tracing::info!(
                content_type = %type_name,
                field = %field_name,
                required = %required,
                "field updated"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/fields")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update field");
            render_server_error("Failed to update field.")
        }
    }
}

/// Handle field deletion.
///
/// POST /admin/structure/types/{type}/fields/{field}/delete
async fn delete_field_submit(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, field_name)): Path<(String, String)>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    match state
        .content_types()
        .delete_field(&type_name, &field_name)
        .await
    {
        Ok(_) => {
            tracing::info!(
                content_type = %type_name,
                field = %field_name,
                "field deleted"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/fields")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to delete field");
            render_server_error("Failed to delete field.")
        }
    }
}

// =============================================================================
// Search Configuration
// =============================================================================

/// The form id of the search switches form.
const SEARCH_SWITCHES_FORM_ID: &str = "search_switches_form";

/// A content type's search switches as stored, defaults when it has none.
async fn stored_search_switches(state: &AppState, type_name: &str) -> SearchSwitches {
    match crate::models::ItemType::find_by_type(state.db(), type_name).await {
        Ok(Some(row)) => SearchSwitches::from_settings(&row.settings),
        Ok(None) => SearchSwitches::default(),
        Err(e) => {
            tracing::warn!(error = %e, content_type = %type_name, "failed to read search switches");
            SearchSwitches::default()
        }
    }
}

/// The two checkboxes, through the kernel's form pipeline.
fn search_switches_form(type_name: &str, switches: SearchSwitches) -> crate::form::Form {
    use crate::form::FormElement;
    crate::form::Form::new(SEARCH_SWITCHES_FORM_ID)
        .action(format!("/admin/structure/types/{type_name}/search"))
        .element(
            "include",
            FormElement::checkbox()
                .title("Include this content type in search")
                .description(
                    "Unchecked, no item of this type is found by site search, in the browser \
                     index or on the server.",
                )
                .default_value(switches.include)
                .weight(0),
        )
        .element(
            "comments",
            FormElement::checkbox()
                .title("Include comments in search")
                .description(
                    "Unchecked, an item of this type is no longer found by words that appear \
                     only in its published comments.",
                )
                .default_value(switches.comments)
                .weight(1),
        )
        .element("op", FormElement::submit("Save search settings").weight(10))
}

/// Manage search field configuration for a content type.
///
/// GET /admin/structure/types/{type}/search
async fn manage_search_config(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let switches = stored_search_switches(&state, &type_name).await;
    render_search_config(&state, &session, &user, &type_name, switches, &[]).await
}

/// The search settings page, its switches form showing `switches`.
async fn render_search_config(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    type_name: &str,
    switches: SearchSwitches,
    errors: &[crate::form::ValidationError],
) -> Response {
    let Some(content_type) = state.content_types().get(type_name) else {
        return render_not_found();
    };

    let switches_form = match super::admin_form::render(
        state,
        session,
        user,
        search_switches_form(type_name, switches),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let flash: Option<String> = session.remove(SEARCH_SWITCHES_FLASH).await.ok().flatten();

    let csrf_token = generate_csrf_token(session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    // Get current search configs
    let search_configs = match state.search().list_field_configs(type_name).await {
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

    // Every text field is searched whether or not it has a weight here; the
    // page says so rather than calling it "not indexed".
    let text_fields: Vec<&str> = content_type
        .fields
        .iter()
        .filter(|f| crate::search::is_text_field(&f.field_type))
        .map(|f| f.field_name.as_str())
        .collect();

    let mut context = tera::Context::new();
    context.insert("content_type", &content_type);
    context.insert("fields", &content_type.fields);
    context.insert("search_configs", &config_map);
    context.insert("text_fields", &text_fields);
    context.insert("switches_form", &switches_form);
    context.insert("flash", &flash);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert(
        "path",
        &format!("/admin/structure/types/{type_name}/search"),
    );

    let viewer = super::helpers::user_context_for(state, user).await;
    render_admin_template(state, &viewer, "admin/search-config.html", context).await
}

/// Where the save confirmation waits for the redirect.
const SEARCH_SWITCHES_FLASH: &str = "search_switches_flash";

/// Save a content type's two search switches.
///
/// POST /admin/structure/types/{type}/search
///
/// Stores them in the type's `settings` (merged, so nothing else there
/// changes), recomputes the type's full text vectors so `/search` follows at
/// once, and asks for a Pagefind rebuild at the next cron run.
async fn save_search_switches(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = crate::content::submission::PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let wanted = SearchSwitches {
        include: posted.contains("include"),
        comments: posted.contains("comments"),
    };
    match super::admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        SEARCH_SWITCHES_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(()) => {}
        Err(Ok(errors)) => {
            return render_search_config(&state, &session, &user, &type_name, wanted, &errors)
                .await;
        }
        Err(Err(response)) => return response,
    }

    let patch = serde_json::json!({ SearchSwitches::SETTINGS_KEY: wanted });
    if let Err(e) = state
        .content_types()
        .update(
            &type_name,
            &content_type.label,
            Some(content_type.description.as_str()),
            patch,
        )
        .await
    {
        tracing::error!(error = %e, content_type = %type_name, "failed to save search switches");
        return render_server_error("The search settings could not be saved.");
    }
    if let Err(e) = state.search().reindex_bundle(&type_name).await {
        tracing::error!(error = %e, content_type = %type_name, "failed to reindex after search switches");
    }
    crate::cron::request_search_index_rebuild(state.db()).await;

    tracing::info!(
        log_type = "config",
        user_id = %user.id,
        content_type = %type_name,
        include = wanted.include,
        comments = wanted.comments,
        "search switches changed"
    );
    let _ = session
        .insert(
            SEARCH_SWITCHES_FLASH,
            "The search settings were saved. The search index is rebuilt at the next cron run.",
        )
        .await;
    Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    // Validate weight
    let weight = form.weight.chars().next().unwrap_or('C');
    if !['A', 'B', 'C', 'D'].contains(&weight) {
        return render_error("Invalid weight. Must be A, B, C, or D.");
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
            Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to configure search field");
            render_server_error("Failed to configure search field.")
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
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
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
            Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to remove search field config");
            render_server_error("Failed to remove search field configuration.")
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
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(_content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    match state.search().reindex_bundle(&type_name).await {
        Ok(count) => {
            tracing::info!(
                content_type = %type_name,
                count = %count,
                "content type reindexed"
            );
            Redirect::to(&format!("/admin/structure/types/{type_name}/search")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to reindex content type");
            render_server_error("Failed to reindex content.")
        }
    }
}

// =============================================================================
// AJAX Handler
// =============================================================================

/// Handle AJAX add_field trigger for manage_fields forms.
pub(crate) async fn handle_ajax_add_field(
    state: &AppState,
    request: &crate::form::AjaxRequest,
) -> Response {
    use crate::form::{AjaxCommand, AjaxResponse};

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
        return Json(AjaxResponse::new().alert(MACHINE_NAME_ERROR)).into_response();
    }
    if field_type.is_empty() {
        return Json(AjaxResponse::new().alert("Field type is required.")).into_response();
    }

    // Add the field
    match state
        .content_types()
        .add_field(type_name, name, label, field_type)
        .await
    {
        Ok(()) => {}
        // Same two user-fixable refusals as the form post, same wording: the
        // registry owns the message.
        Err(
            e @ (AddFieldError::DuplicateFieldName { .. } | AddFieldError::UnknownFieldType { .. }),
        ) => {
            return Json(AjaxResponse::new().alert(e.to_string())).into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to add field via AJAX");
            return Json(AjaxResponse::new().alert("Failed to add field.")).into_response();
        }
    }

    tracing::info!(content_type = %type_name, field = %name, "field added via AJAX");

    // Build the new row HTML
    let row_html = format!(
        r#"<tr data-field="{name_esc}">
            <td>{label_esc}</td>
            <td><code>{name_esc}</code></td>
            <td>{type_esc}</td>
            <td>No</td>
            <td>
                <a href="/admin/structure/types/{ct_esc}/fields/{name_esc}/edit">Edit</a>
            </td>
        </tr>"#,
        name_esc = html_escape(name),
        label_esc = html_escape(label),
        type_esc = html_escape(field_type),
        ct_esc = html_escape(type_name),
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
// Router
// =============================================================================

/// Build the content type admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/structure/types", get(list_content_types))
        .route(
            "/admin/structure/types/add",
            get(add_content_type_form).post(add_content_type_submit),
        )
        .route(
            "/admin/structure/types/{type}/edit",
            get(edit_content_type_form).post(edit_content_type_submit),
        )
        .route("/admin/structure/types/{type}/fields", get(manage_fields))
        .route("/admin/structure/types/{type}/fields/add", post(add_field))
        .route(
            "/admin/structure/types/{type}/fields/{field}/edit",
            get(edit_field_form).post(edit_field_submit),
        )
        .route(
            "/admin/structure/types/{type}/fields/{field}/delete",
            post(delete_field_submit),
        )
        .route(
            "/admin/structure/types/{type}/search",
            get(manage_search_config).post(save_search_switches),
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
}
