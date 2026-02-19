//! Admin routes for content type management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, html_escape, is_valid_machine_name, render_admin_template, render_error,
    render_not_found, render_server_error, require_admin, require_csrf,
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/structure/types");

    render_admin_template(&state, "admin/content-types.html", context).await
}

/// Show add content type form.
///
/// GET /admin/structure/types/add
async fn add_content_type_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
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

    render_admin_template(&state, "admin/content-type-form.html", context).await
}

/// Handle add content type form submission.
///
/// POST /admin/structure/types/add
async fn add_content_type_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ContentTypeFormData>,
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

        return render_admin_template(&state, "admin/content-type-form.html", context).await;
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
            render_server_error("Failed to create content type.")
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
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
    let (title_label, published_default, revision_default) = match &db_type {
        Some(it) => (
            it.title_label.as_deref().unwrap_or("Title"),
            it.settings
                .get("published_default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            it.settings
                .get("revision_default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        ),
        None => ("Title", false, false),
    };

    context.insert(
        "values",
        &serde_json::json!({
            "label": content_type.label,
            "machine_name": content_type.machine_name,
            "description": content_type.description,
            "title_label": title_label,
            "published_default": published_default,
            "revision_default": revision_default,
        }),
    );
    context.insert("path", &format!("/admin/structure/types/{type_name}/edit"));

    render_admin_template(&state, "admin/content-type-form.html", context).await
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
    if let Err(redirect) = require_admin(&state, &session).await {
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
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
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
        context.insert("path", &format!("/admin/structure/types/{type_name}/edit"));

        return render_admin_template(&state, "admin/content-type-form.html", context).await;
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

    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
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

    render_admin_template(&state, "admin/field-list.html", context).await
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
    if let Err(redirect) = require_admin(&state, &session).await {
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
        errors.push("Machine name must start with a letter and contain only lowercase letters, numbers, and underscores.".to_string());
    }

    if form.field_type.is_empty() {
        errors.push("Field type is required.".to_string());
    }

    if !errors.is_empty() {
        let Some(content_type) = state.content_types().get(&type_name) else {
            return render_not_found();
        };
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        // Save form state for AJAX callbacks
        let form_state = crate::form::FormState::new(
            format!("manage_fields_{type_name}"),
            form_build_id.clone(),
        );
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

        return render_admin_template(&state, "admin/field-list.html", context).await;
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
        Err(e) => {
            tracing::error!(error = %e, "failed to add field");
            render_server_error("Failed to add field.")
        }
    }
}

// =============================================================================
// Search Configuration
// =============================================================================

/// Manage search field configuration for a content type.
///
/// GET /admin/structure/types/{type}/search
async fn manage_search_config(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
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
        &format!("/admin/structure/types/{type_name}/search"),
    );

    render_admin_template(&state, "admin/search-config.html", context).await
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
    if let Err(redirect) = require_admin(&state, &session).await {
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
    if let Err(redirect) = require_admin(&state, &session).await {
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
    if let Err(redirect) = require_admin(&state, &session).await {
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
}
