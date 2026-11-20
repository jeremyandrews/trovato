//! Admin routes for content item management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::csrf::generate_csrf_token;
use crate::models::CreateItem;
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, admin_user_context, build_local_tasks, html_escape, render_admin_template,
    render_error, render_forbidden, render_not_found, render_server_error, require_csrf,
    require_permission, single_form_field,
};

/// Session key for flash messages on the content list page.
const CONTENT_FLASH_KEY: &str = "content_admin_flash";

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

/// List all content.
///
/// GET /admin/content
async fn list_content(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "edit any content").await {
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

    // The targets the bulk move offers. Every stage: whether a user may move an
    // item there is decided per item when the action runs.
    let stages = match state.items().stage_view().await {
        Ok(view) => view.stages().to_vec(),
        Err(e) => {
            tracing::warn!(error = %e, "stage list for the bulk move failed");
            Vec::new()
        }
    };

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
    context.insert("stages", &stages);
    context.insert("type_filter", &type_filter.unwrap_or(""));
    context.insert(
        "status_filter",
        &status_filter.map(|s| s.to_string()).unwrap_or_default(),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("flash", &flash);
    context.insert("path", "/admin/content");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-list.html",
        context,
    )
    .await
}

/// Select content type before adding.
///
/// GET /admin/content/add
async fn select_content_type(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "create content").await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/content/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-add-select.html",
        context,
    )
    .await
}

/// Show add content form.
///
/// GET /admin/content/add/{type}
async fn add_content_form(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    // The per-type permission `/item/add/{type}` already checks, so the two
    // ways into the same form agree. Gating this on `is_admin` was what made a
    // role holding a plugin's own content permissions unable to use the admin
    // UI to create that plugin's content.
    let user =
        match require_permission(&state, &session, &format!("create {type_name} content")).await {
            Ok(user) => user,
            Err(redirect) => return redirect,
        };

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let user_ctx = admin_user_context(&state, &user).await;
    render_content_form(&state, &session, &user_ctx, &content_type, None, None, &[]).await
}

/// Render the admin content form, add or edit, with whatever posted values
/// and errors the caller has.
///
/// One body for every caller — the GETs, a validation failure, and a write
/// the field or publish gate refused — so they cannot disagree about what the
/// page shows. The form itself comes from `item_form::build`, the pipeline
/// `/item/add` and `/item/{id}/edit` use too; this adds the admin chrome.
async fn render_content_form(
    state: &AppState,
    session: &Session,
    user_ctx: &crate::tap::UserContext,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    item: Option<&crate::models::Item>,
    posted: Option<&PostedForm>,
    errors: &[crate::form::ValidationError],
) -> Response {
    let type_name = &content_type.machine_name;
    let path = match item {
        Some(item) => format!("/admin/content/{}/edit", item.id),
        None => format!("/admin/content/add/{type_name}"),
    };
    let mut extra = vec![(
        "cancel".to_string(),
        crate::form::FormElement::markup(
            r#"<a href="/admin/content" class="button button--secondary">Cancel</a>"#,
        )
        .weight(crate::content::SUBMIT_WEIGHT + 1),
    )];
    if let Some(item) = item {
        extra.extend(super::item_form::stage_element(state, user_ctx, item).await);
    }
    let mut form =
        match super::item_form::build(state, session, user_ctx, content_type, item, &path, extra)
            .await
        {
            Ok(form) => form,
            Err(e) => {
                tracing::error!(error = %e, "failed to build the content form");
                return render_server_error("Failed to build the form.");
            }
        };
    if let Some(posted) = posted {
        super::item_form::redisplay(&mut form, posted);
    }
    let form_html = match state.theme().render_form_with_errors(&form, errors) {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to render the content form");
            return render_server_error("Failed to render the form.");
        }
    };

    let mut context = tera::Context::new();
    context.insert("editing", &item.is_some());
    context.insert("content_type", content_type);
    context.insert("form_html", &form_html);
    context.insert("path", &path);
    if let Some(item) = item {
        let item_id = item.id;
        // Local task tabs for item edit pages (hardcoded + plugin-registered)
        context.insert(
            "local_tasks",
            &build_local_tasks(
                state,
                "/admin/content/:id",
                &path,
                Some(&item_id.to_string()),
                vec![
                    serde_json::json!({"title": "View", "path": format!("/item/{item_id}"), "active": false}),
                    serde_json::json!({"title": "Edit", "path": &path, "active": true}),
                    serde_json::json!({"title": "Revisions", "path": format!("/item/{item_id}/revisions"), "active": false}),
                ],
            ),
        );
    }

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/content-form.html",
        context,
    )
    .await
}

/// Handle add content form submission.
///
/// POST /admin/content/add/{type}
async fn add_content_submit(
    State(state): State<AppState>,
    session: Session,
    Extension(resolved_lang): Extension<crate::middleware::language::ResolvedLanguage>,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    // Same per-type permission the GET above checks, and the same one
    // `/item/add/{type}` checks.
    let user =
        match require_permission(&state, &session, &format!("create {type_name} content")).await {
            Ok(user) => user,
            Err(redirect) => return redirect,
        };

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let user_ctx = admin_user_context(&state, &user).await;

    let (form_state, fields_json) =
        match super::item_form::process(&state, &session, &user_ctx, &content_type, None, &posted)
            .await
        {
            Ok(processed) => processed,
            Err(crate::form::ProcessError::Csrf) => {
                return render_error("Invalid or expired form token. Please try again.");
            }
            Err(crate::form::ProcessError::Invalid(errors)) => {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    None,
                    Some(&posted),
                    &errors,
                )
                .await;
            }
        };

    let file_ids = extract_file_ids(&state, &fields_json, &type_name);
    let input = CreateItem {
        item_type: type_name.clone(),
        title: posted.get("title").unwrap_or_default().trim().to_string(),
        author_id: user.id,
        status: requested_status(&posted),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        stage_id: None,
        language: Some(resolved_lang.0),
        log: Some("Created via admin UI".to_string()),
    };

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

            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    Some(item.id),
                    &super::item_form::tap_state(&state, &user_ctx),
                )
                .await;

            tracing::info!(item_id = %item.id, "content created");

            // Flash success message with link to edit the created item
            let msg = format!(
                "{} <a href=\"/admin/content/{}/edit\">{}</a> has been created.",
                html_escape(&content_type.label),
                item.id,
                html_escape(&item.title),
            );
            if let Err(e) = session.insert(CONTENT_FLASH_KEY, &msg).await {
                tracing::warn!(error = %e, "failed to set flash message");
            }

            Redirect::to("/admin/content").into_response()
        }
        Err(e) => {
            // A write the field or publish gate refused is the user's to fix,
            // so the form comes back with the reason on it rather than a 500.
            if let Some(denied) = e.downcast_ref::<crate::content::WriteDenied>() {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    None,
                    Some(&posted),
                    &[crate::form::ValidationError::form(denied.to_string())],
                )
                .await;
            }
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
    let user = match require_permission(&state, &session, "edit any content").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };

    let user_ctx = admin_user_context(&state, &user).await;

    // `edit any content` admits a user to the screen; whether they may edit
    // this item is the access seam's decision, a plugin's Deny included. Asked
    // before anything of the item is rendered, and answered as the submit is.
    match state.items().check_access(&item, "edit", &user_ctx).await {
        Ok(true) => {}
        Ok(false) => return render_forbidden("You may not edit this item."),
        Err(e) => {
            tracing::error!(error = %e, item_id = %item_id, "edit access check failed");
            return render_server_error("Could not check access to this item.");
        }
    }

    render_content_form(
        &state,
        &session,
        &user_ctx,
        &content_type,
        Some(&item),
        None,
        &[],
    )
    .await
}

/// The status a submission asks for, or `None` when it said nothing.
///
/// An unchecked checkbox posts nothing, so the form stamps `_status_rendered`
/// beside the published control it renders for a user who may publish.
/// Without the marker an absent `status` and a form that never offered one
/// are the same request, and `None` goes to the service as "leave it".
fn requested_status(posted: &PostedForm) -> Option<i16> {
    posted
        .contains("_status_rendered")
        .then(|| i16::from(posted.contains("status")))
}

/// The stage a submission asks to move the item to: `Ok(None)` when it asked
/// for none, `Err` when the value is not a stage id. Whether the move is
/// allowed is `ItemService::update`'s decision.
fn requested_stage(posted: &PostedForm) -> Result<Option<uuid::Uuid>, ()> {
    match posted.get("_stage").map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => uuid::Uuid::parse_str(value).map(Some).map_err(|_| ()),
    }
}

/// Handle edit content form submission.
///
/// POST /admin/content/{id}/edit
async fn edit_content_submit(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "edit any content").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };
    let user_ctx = admin_user_context(&state, &user).await;

    // Built over the stored fields from the list the form rendered, so a key
    // it did not show is kept rather than erased.
    let (form_state, fields_json) = match super::item_form::process(
        &state,
        &session,
        &user_ctx,
        &content_type,
        Some(&item),
        &posted,
    )
    .await
    {
        Ok(processed) => processed,
        Err(crate::form::ProcessError::Csrf) => {
            return render_error("Invalid or expired form token. Please try again.");
        }
        Err(crate::form::ProcessError::Invalid(errors)) => {
            return render_content_form(
                &state,
                &session,
                &user_ctx,
                &content_type,
                Some(&item),
                Some(&posted),
                &errors,
            )
            .await;
        }
    };

    let Ok(stage_id) = requested_stage(&posted) else {
        return render_error("The form could not be read.");
    };

    let file_ids = extract_file_ids(&state, &fields_json, &item.item_type);
    let input = crate::models::UpdateItem {
        stage_id,
        title: posted.get("title").map(|t| t.trim().to_string()),
        status: requested_status(&posted),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        log: Some("Updated via admin UI".to_string()),
    };

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

            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    Some(item_id),
                    &super::item_form::tap_state(&state, &user_ctx),
                )
                .await;

            tracing::info!(item_id = %item_id, "content updated");

            if let Some(ref updated_item) = updated {
                let msg = format!(
                    "{} <a href=\"/admin/content/{}/edit\">{}</a> has been updated.",
                    html_escape(&content_type.label),
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
            // As on the add path: a gate refusal belongs on the form, not in a
            // 500.
            if let Some(denied) = e.downcast_ref::<crate::content::WriteDenied>() {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    Some(&item),
                    Some(&posted),
                    &[crate::form::ValidationError::form(denied.to_string())],
                )
                .await;
            }
            // The item-level refusal from `ItemService::update`: a 403, as the
            // form answers, rather than a 500.
            if e.to_string().contains("access denied") {
                return render_forbidden("You may not edit this item.");
            }
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
    let user = match require_permission(&state, &session, "delete any content").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let user_ctx = admin_user_context(&state, &user).await;
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

/// Bulk action form data, read from the body the content list screen posts.
///
/// The screen posts one `ids[]` pair per checked box. That shape is read by
/// [`BulkActionForm::parse`] rather than by `axum::Form`: `Form` deserializes
/// with `serde_urlencoded`, which does not gather repeated keys into a
/// sequence, so a `Vec` field fed by `ids[]` refused every request that
/// selected anything, with a 422, before the handler ran.
#[derive(Debug, PartialEq)]
struct BulkActionForm {
    token: String,
    action: String,
    ids: Vec<uuid::Uuid>,
    /// The target of `move_to_stage`, as sent; read only by that action.
    stage: Option<String>,
}

/// Why a bulk action body was not turned into a [`BulkActionForm`].
#[derive(Debug, PartialEq)]
enum BulkFormError {
    /// `_token` or `action` is missing or appears more than once. Refused with
    /// a 400 before any permission lookup: there is no single action to gate.
    Malformed(&'static str),
    /// An `ids[]` value is not a UUID. The token and action are carried so the
    /// handler can still run its permission and CSRF checks before refusing
    /// the whole selection.
    InvalidId {
        token: String,
        action: String,
        value: String,
    },
}

impl BulkActionForm {
    /// Parse an `application/x-www-form-urlencoded` bulk action body.
    ///
    /// `_token` and `action` must each appear exactly once (the same rule
    /// [`single_form_field`] applies to the CSRF token everywhere else). Every
    /// `ids[]` value must be a UUID; a repeated id is kept once, at the
    /// position it was first seen. No `ids[]` at all is an empty selection.
    fn parse(body: &str) -> Result<Self, BulkFormError> {
        let token = single_form_field(body, "_token")
            .ok_or(BulkFormError::Malformed("missing or repeated _token"))?;
        let action = single_form_field(body, "action")
            .ok_or(BulkFormError::Malformed("missing or repeated action"))?;

        let mut ids: Vec<uuid::Uuid> = Vec::new();
        for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
            if key != "ids[]" {
                continue;
            }
            let Ok(id) = uuid::Uuid::parse_str(&value) else {
                return Err(BulkFormError::InvalidId {
                    token,
                    action,
                    value: value.into_owned(),
                });
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let stage = single_form_field(body, "stage");
        Ok(Self {
            token,
            action,
            ids,
            stage,
        })
    }
}

/// Bulk operations on content items.
///
/// POST /admin/content/bulk
async fn bulk_content_action(
    State(state): State<AppState>,
    session: Session,
    body: axum::body::Bytes,
) -> Response {
    let Ok(body) = std::str::from_utf8(&body) else {
        return render_error("The form could not be read.");
    };
    // An unreadable id does not refuse the request here: the permission and
    // CSRF checks still run first, and the selection is refused below them,
    // before any write.
    let (form, invalid_id) = match BulkActionForm::parse(body) {
        Ok(form) => (form, None),
        Err(BulkFormError::InvalidId {
            token,
            action,
            value,
        }) => (
            BulkActionForm {
                token,
                action,
                ids: Vec::new(),
                stage: None,
            },
            Some(value),
        ),
        Err(BulkFormError::Malformed(reason)) => {
            tracing::debug!(reason, "bulk action body refused");
            return render_error("The form could not be read.");
        }
    };

    // A bulk action is gated on the permissions for the action it performs, not
    // on one permission for the whole screen: bulk delete is a delete, and a
    // role that may publish must not get a delete for free by routing it
    // through this endpoint. Bulk publish and unpublish each change an item's
    // published state, so they need `publish content` on top of the edit
    // permission — the same answer `ItemService::gate_publish` would give one
    // item at a time, asked once here so the screen refuses rather than
    // reporting every item as failed. A move to another stage is an edit, and
    // each item's move is then decided by the workflow in
    // `ItemService::update`. An unrecognized action asks for the edit
    // permission and is then rejected as unknown below.
    let required: &[&str] = match form.action.as_str() {
        "delete" => &["delete any content"],
        "publish" | "unpublish" => &["edit any content", crate::content::PUBLISH_CONTENT],
        _ => &["edit any content"],
    };
    let mut holder = None;
    for permission in required {
        match require_permission(&state, &session, permission).await {
            Ok(user) => holder = Some(user),
            Err(redirect) => return redirect,
        }
    }
    let Some(user) = holder else {
        // Unreachable: every arm above names at least one permission.
        return render_server_error("No permission to check for this action.");
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate action before processing
    if !matches!(
        form.action.as_str(),
        "publish" | "unpublish" | "delete" | "move_to_stage"
    ) {
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

    // Never act on part of a selection the browser did not send cleanly. The
    // value is echoed escaped and cut short: it is whatever the client sent.
    if let Some(value) = invalid_id {
        let shown: String = value.chars().take(64).collect();
        if let Err(e) = session
            .insert(
                CONTENT_FLASH_KEY,
                format!(
                    "Nothing was changed: \"{}\" is not a valid item id.",
                    html_escape(&shown)
                ),
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

    // A move needs a target, and the target must be a stage id. Refused whole,
    // before any write; whether the stage exists, and whether this user may
    // move each item there, is the service's decision per item.
    let target_stage = if form.action == "move_to_stage" {
        match form
            .stage
            .as_deref()
            .and_then(|v| uuid::Uuid::parse_str(v.trim()).ok())
        {
            Some(stage) => Some(stage),
            None => {
                if let Err(e) = session
                    .insert(CONTENT_FLASH_KEY, "Nothing was moved: choose a stage.")
                    .await
                {
                    tracing::warn!(error = %e, "failed to set flash message");
                }
                return Redirect::to("/admin/content").into_response();
            }
        }
    } else {
        None
    };

    let user_ctx = admin_user_context(&state, &user).await;

    let mut success_count = 0u32;
    let mut fail_count = 0u32;
    match form.action.as_str() {
        "publish" | "unpublish" => {
            let new_status: i16 = if form.action == "publish" { 1 } else { 0 };
            for id in &form.ids {
                let update = crate::models::UpdateItem {
                    stage_id: None,
                    title: None,
                    status: Some(new_status),
                    fields: None,
                    promote: None,
                    sticky: None,
                    log: Some(format!("Bulk {}", form.action)),
                };
                // `Ok(None)` is an id that names no item: a failure, as the
                // delete arm's `Ok(false)` is, not a success.
                match state.items().update(*id, update, &user_ctx).await {
                    Ok(Some(_)) => success_count += 1,
                    Ok(None) => fail_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk action failed");
                        fail_count += 1;
                    }
                }
            }
        }
        "move_to_stage" => {
            for id in &form.ids {
                let update = crate::models::UpdateItem {
                    stage_id: target_stage,
                    title: None,
                    status: None,
                    fields: None,
                    promote: None,
                    sticky: None,
                    log: Some("Bulk move to stage".to_string()),
                };
                match state.items().update(*id, update, &user_ctx).await {
                    Ok(Some(_)) => success_count += 1,
                    Ok(None) => fail_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk move failed");
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
        "move_to_stage" => "moved",
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const A: &str = "0190a0b0-0000-7000-8000-00000000000a";
    const B: &str = "0190a0b0-0000-7000-8000-00000000000b";
    const C: &str = "0190a0b0-0000-7000-8000-00000000000c";

    fn uuid(s: &str) -> uuid::Uuid {
        uuid::Uuid::parse_str(s).unwrap()
    }

    #[test]
    fn several_ids_are_read_in_the_order_they_were_sent() {
        let body = format!("_token=t&action=publish&ids%5B%5D={B}&ids%5B%5D={A}&ids[]={C}");
        let form = BulkActionForm::parse(&body).unwrap();
        assert_eq!(form.ids, vec![uuid(B), uuid(A), uuid(C)]);
        assert_eq!(form.action, "publish");
        assert_eq!(form.token, "t");
    }

    #[test]
    fn a_token_with_reserved_characters_round_trips_exactly() {
        // `+`, `/` and `=` percent encoded, and a literal `+` for a space, as a
        // browser encodes the form.
        let body = format!("_token=a%2Bb%2Fc%3D%3D+d&action=delete&ids%5B%5D={A}");
        let form = BulkActionForm::parse(&body).unwrap();
        assert_eq!(form.token, "a+b/c== d");
        assert_eq!(form.action, "delete");
    }

    #[test]
    fn an_unreadable_id_is_an_error_that_keeps_the_token_and_action() {
        let body = format!("_token=t&action=publish&ids%5B%5D={A}&ids%5B%5D=not-a-uuid");
        assert_eq!(
            BulkActionForm::parse(&body),
            Err(BulkFormError::InvalidId {
                token: "t".to_string(),
                action: "publish".to_string(),
                value: "not-a-uuid".to_string(),
            })
        );
    }

    #[test]
    fn a_repeated_id_is_kept_once_at_its_first_position() {
        let body = format!("_token=t&action=publish&ids[]={A}&ids[]={B}&ids[]={A}");
        let form = BulkActionForm::parse(&body).unwrap();
        assert_eq!(form.ids, vec![uuid(A), uuid(B)]);
    }

    #[test]
    fn a_repeated_token_or_action_is_malformed() {
        assert!(matches!(
            BulkActionForm::parse("_token=t&_token=t&action=publish"),
            Err(BulkFormError::Malformed(_))
        ));
        assert!(matches!(
            BulkActionForm::parse("_token=t&action=publish&action=delete"),
            Err(BulkFormError::Malformed(_))
        ));
    }

    #[test]
    fn a_missing_token_or_action_is_malformed() {
        assert!(matches!(
            BulkActionForm::parse("action=publish"),
            Err(BulkFormError::Malformed(_))
        ));
        assert!(matches!(
            BulkActionForm::parse("_token=t"),
            Err(BulkFormError::Malformed(_))
        ));
    }

    #[test]
    fn no_ids_is_an_empty_selection() {
        let form = BulkActionForm::parse("_token=t&action=unpublish").unwrap();
        assert!(form.ids.is_empty());
    }
}
