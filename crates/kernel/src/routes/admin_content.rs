//! Admin routes for content item management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::CreateItem;
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, admin_user_context, build_local_tasks, html_escape, render_admin_template,
    render_not_found, render_server_error, require_admin, require_csrf,
};

/// Session key for flash messages on the content list page.
const CONTENT_FLASH_KEY: &str = "content_admin_flash";

/// Extract content fields from form data, excluding system fields.
fn extract_content_fields(
    fields: &std::collections::HashMap<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut result = serde_json::Map::new();
    for (key, value) in fields {
        if !key.starts_with('_') && key != "title" && key != "status" && key != "log" {
            result.insert(key.clone(), value.clone());
        }
    }
    result
}

/// Extract file UUIDs from item fields.
///
/// File fields store a UUID string referencing `file_managed.id`. Call this
/// *before* moving `fields_json` into a `CreateItem`/`UpdateItem`, then pass
/// the returned IDs to [`promote_file_ids`] after the save succeeds.
fn extract_file_ids(
    state: &AppState,
    fields: &serde_json::Map<String, serde_json::Value>,
    content_type_name: &str,
) -> Vec<uuid::Uuid> {
    let Some(ct) = state.content_types().get(content_type_name) else {
        return Vec::new();
    };

    let mut file_ids = Vec::new();
    for field_def in &ct.fields {
        if matches!(field_def.field_type, trovato_sdk::types::FieldType::File)
            && let Some(val) = fields.get(&field_def.field_name)
            && let Some(id_str) = val.as_str()
            && let Ok(id) = id_str.parse::<uuid::Uuid>()
        {
            file_ids.push(id);
        }
    }
    file_ids
}

/// Promote extracted file IDs to permanent status.
///
/// Temporary uploads must be promoted so the cleanup cron does not delete them.
async fn promote_file_ids(state: &AppState, file_ids: &[uuid::Uuid]) {
    if file_ids.is_empty() {
        return;
    }
    match state.files().mark_permanent_batch(file_ids).await {
        Ok(count) => {
            if count > 0 {
                tracing::debug!(count, "promoted file(s) to permanent");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to promote files to permanent");
        }
    }
}

/// Content form data.
#[derive(Debug, Deserialize)]
struct ContentFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    title: String,
    status: Option<String>,
    #[serde(flatten)]
    fields: std::collections::HashMap<String, serde_json::Value>,
}

/// List all content.
///
/// GET /admin/content
async fn list_content(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let type_filter = params.get("type").map(|s| s.as_str());
    let status_filter = params.get("status").and_then(|s| s.parse::<i16>().ok());

    let items = match state
        .items()
        .list_filtered(type_filter, status_filter, None, 100, 0)
        .await
    {
        Ok((items, _total)) => items,
        Err(e) => {
            tracing::error!(error = %e, "failed to list content");
            return render_server_error("Failed to load content.");
        }
    };

    // Get authors for display
    let mut authors: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in &items {
        if !authors.contains_key(&item.author_id.to_string())
            && let Ok(Some(user)) = state.users().find_by_id(item.author_id).await
        {
            authors.insert(item.author_id.to_string(), user.name);
        }
    }

    let content_types = state.content_types().list_all().await;

    let csrf_token = generate_csrf_token(&session).await;

    // Read and clear flash message
    let flash: Option<String> = session.get(CONTENT_FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(CONTENT_FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear flash message");
    }

    let mut context = tera::Context::new();
    context.insert("items", &items);
    context.insert("authors", &authors);
    context.insert("content_types", &content_types);
    context.insert("type_filter", &type_filter.unwrap_or(""));
    context.insert(
        "status_filter",
        &status_filter.map(|s| s.to_string()).unwrap_or_default(),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("flash", &flash);
    context.insert("path", "/admin/content");

    render_admin_template(&state, "admin/content-list.html", context).await
}

/// Select content type before adding.
///
/// GET /admin/content/add
async fn select_content_type(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/content/add");

    render_admin_template(&state, "admin/content-add-select.html", context).await
}

/// Show add content form.
///
/// GET /admin/content/add/{type}
async fn add_content_form(
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

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/content/add/{type_name}"));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("content_type", &content_type);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", &format!("/admin/content/add/{type_name}"));

    render_admin_template(&state, "admin/content-form.html", context).await
}

/// Handle add content form submission.
///
/// POST /admin/content/add/{type}
async fn add_content_submit(
    State(state): State<AppState>,
    session: Session,
    Extension(resolved_lang): Extension<crate::middleware::language::ResolvedLanguage>,
    Path(type_name): Path<String>,
    Form(form): Form<ContentFormData>,
) -> Response {
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let mut fields_json = extract_content_fields(&form.fields);

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
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/content/add/{type_name}"));
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
        context.insert("path", &format!("/admin/content/add/{type_name}"));

        return render_admin_template(&state, "admin/content-form.html", context).await;
    }

    let file_ids = extract_file_ids(&state, &fields_json, &type_name);
    let input = CreateItem {
        item_type: type_name.clone(),
        title: form.title.clone(),
        author_id: user.id,
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        stage_id: None,
        language: Some(resolved_lang.0),
        log: Some("Created via admin UI".to_string()),
    };

    let user_ctx = admin_user_context(&user);
    match state.items().create(input, &user_ctx).await {
        Ok(item) => {
            // Promote temporary file uploads to permanent
            promote_file_ids(&state, &file_ids).await;

            // Auto-generate URL alias if pattern configured for this type
            if let Err(e) = crate::services::pathauto::auto_alias_item(
                state.db(),
                item.id,
                &item.title,
                &item.item_type,
                item.created,
            )
            .await
            {
                tracing::warn!(error = %e, item_id = %item.id, "pathauto alias generation failed");
            }

            tracing::info!(item_id = %item.id, "content created");

            // Flash success message with link to edit the created item
            let type_label = state
                .content_types()
                .get(&item.item_type)
                .map(|ct| ct.label)
                .unwrap_or_else(|| item.item_type.clone());
            let msg = format!(
                "{} <a href=\"/admin/content/{}/edit\">{}</a> has been created.",
                html_escape(&type_label),
                item.id,
                html_escape(&item.title),
            );
            if let Err(e) = session.insert(CONTENT_FLASH_KEY, &msg).await {
                tracing::warn!(error = %e, "failed to set flash message");
            }

            Redirect::to("/admin/content").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create content");
            render_server_error("Failed to create content.")
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
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/content/{item_id}/edit"));
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
    context.insert("path", &format!("/admin/content/{item_id}/edit"));

    // Local task tabs for item edit pages (hardcoded + plugin-registered)
    let current_path = format!("/admin/content/{item_id}/edit");
    context.insert(
        "local_tasks",
        &build_local_tasks(
            &state,
            "/admin/content/:id",
            &current_path,
            Some(&item_id.to_string()),
            vec![
                serde_json::json!({"title": "View", "path": format!("/item/{item_id}"), "active": false}),
                serde_json::json!({"title": "Edit", "path": current_path, "active": true}),
                serde_json::json!({"title": "Revisions", "path": format!("/item/{item_id}/revisions"), "active": false}),
            ],
        ),
    );

    render_admin_template(&state, "admin/content-form.html", context).await
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
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };

    // Validate
    let mut errors = Vec::new();

    if form.title.trim().is_empty() {
        errors.push("Title is required.".to_string());
    }

    let mut fields_json = extract_content_fields(&form.fields);

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
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/content/{item_id}/edit"));
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
        let current_path = format!("/admin/content/{item_id}/edit");
        context.insert("path", &current_path);
        context.insert(
            "local_tasks",
            &build_local_tasks(
                &state,
                "/admin/content/:id",
                &current_path,
                Some(&item_id.to_string()),
                vec![
                    serde_json::json!({"title": "View", "path": format!("/item/{item_id}"), "active": false}),
                    serde_json::json!({"title": "Edit", "path": &current_path, "active": true}),
                    serde_json::json!({"title": "Revisions", "path": format!("/item/{item_id}/revisions"), "active": false}),
                ],
            ),
        );

        return render_admin_template(&state, "admin/content-form.html", context).await;
    }

    let file_ids = extract_file_ids(&state, &fields_json, &item.item_type);
    let input = crate::models::UpdateItem {
        title: Some(form.title.clone()),
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        log: Some("Updated via admin UI".to_string()),
    };

    let user_ctx = admin_user_context(&user);
    match state.items().update(item_id, input, &user_ctx).await {
        Ok(updated) => {
            // Promote temporary file uploads to permanent
            promote_file_ids(&state, &file_ids).await;

            // Auto-update URL alias from pathauto pattern
            if let Some(ref updated_item) = updated
                && let Err(e) = crate::services::pathauto::update_alias_item(
                    state.db(),
                    updated_item.id,
                    &updated_item.title,
                    &updated_item.item_type,
                    updated_item.created,
                )
                .await
            {
                tracing::warn!(error = %e, item_id = %item_id, "pathauto alias update failed");
            }

            tracing::info!(item_id = %item_id, "content updated");

            if let Some(ref updated_item) = updated {
                let type_label = state
                    .content_types()
                    .get(&updated_item.item_type)
                    .map(|ct| ct.label)
                    .unwrap_or_else(|| updated_item.item_type.clone());
                let msg = format!(
                    "{} <a href=\"/admin/content/{}/edit\">{}</a> has been updated.",
                    html_escape(&type_label),
                    item_id,
                    html_escape(&updated_item.title),
                );
                if let Err(e) = session.insert(CONTENT_FLASH_KEY, &msg).await {
                    tracing::warn!(error = %e, "failed to set flash message");
                }
            }

            Redirect::to("/admin/content").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update content");
            render_server_error("Failed to update content.")
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
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let user_ctx = admin_user_context(&user);
    match state.items().delete(item_id, &user_ctx).await {
        Ok(true) => {
            tracing::info!(item_id = %item_id, "content deleted");
            if let Err(e) = session
                .insert(CONTENT_FLASH_KEY, "Content has been deleted.")
                .await
            {
                tracing::warn!(error = %e, "failed to set flash message");
            }
            Redirect::to("/admin/content").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete content");
            render_server_error("Failed to delete content.")
        }
    }
}

/// Bulk action form data.
#[derive(Debug, Deserialize)]
struct BulkActionForm {
    #[serde(rename = "_token")]
    token: String,
    action: String,
    #[serde(rename = "ids[]", default)]
    ids: Vec<uuid::Uuid>,
}

/// Bulk operations on content items.
///
/// POST /admin/content/bulk
async fn bulk_content_action(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<BulkActionForm>,
) -> Response {
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate action before processing
    if !matches!(form.action.as_str(), "publish" | "unpublish" | "delete") {
        if let Err(e) = session
            .insert(
                CONTENT_FLASH_KEY,
                format!("Unknown action: {}", html_escape(&form.action)),
            )
            .await
        {
            tracing::warn!(error = %e, "failed to set flash message");
        }
        return Redirect::to("/admin/content").into_response();
    }

    if form.ids.is_empty() {
        if let Err(e) = session
            .insert(CONTENT_FLASH_KEY, "No items selected.")
            .await
        {
            tracing::warn!(error = %e, "failed to set flash message");
        }
        return Redirect::to("/admin/content").into_response();
    }

    let user_ctx = admin_user_context(&user);

    let mut success_count = 0u32;
    let mut fail_count = 0u32;
    match form.action.as_str() {
        "publish" | "unpublish" => {
            let new_status: i16 = if form.action == "publish" { 1 } else { 0 };
            for id in &form.ids {
                let update = crate::models::UpdateItem {
                    title: None,
                    status: Some(new_status),
                    fields: None,
                    promote: None,
                    sticky: None,
                    log: Some(format!("Bulk {}", form.action)),
                };
                match state.items().update(*id, update, &user_ctx).await {
                    Ok(_) => success_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk action failed");
                        fail_count += 1;
                    }
                }
            }
        }
        "delete" => {
            for id in &form.ids {
                match state.items().delete(*id, &user_ctx).await {
                    Ok(true) => success_count += 1,
                    Ok(false) => fail_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk delete failed");
                        fail_count += 1;
                    }
                }
            }
        }
        _ => unreachable!(), // Validated above
    }

    let action_label = match form.action.as_str() {
        "publish" => "published",
        "unpublish" => "unpublished",
        "delete" => "deleted",
        _ => unreachable!(),
    };
    let msg = if fail_count > 0 {
        format!("{success_count} item(s) {action_label}. {fail_count} item(s) failed.")
    } else {
        format!("{success_count} item(s) {action_label}.")
    };
    if let Err(e) = session.insert(CONTENT_FLASH_KEY, msg).await {
        tracing::warn!(error = %e, "failed to set flash message");
    }

    Redirect::to("/admin/content").into_response()
}

/// Build admin content routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/content", get(list_content))
        .route("/admin/content/add", get(select_content_type))
        .route(
            "/admin/content/add/{type}",
            get(add_content_form).post(add_content_submit),
        )
        .route(
            "/admin/content/{id}/edit",
            get(edit_content_form).post(edit_content_submit),
        )
        .route("/admin/content/{id}/delete", post(delete_content))
        .route("/admin/content/bulk", post(bulk_content_action))
}
