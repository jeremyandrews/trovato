//! The media library and the media picker's JSON endpoints (BL-115 / B290).
//!
//! `GET /admin/content/media` lists every item of a registered media source
//! (`state.media_sources()`, populated from plugins' `tap_media_source`), with
//! the same filter/sort/pager shape every admin listing uses
//! (`routes::admin_list`). `GET /media` is the same page under the path the
//! `trovato_media` plugin's `tap_menu` names — previously a `MenuDefinition`
//! with no route behind it at all (BL-115); it is a real route now, not a
//! redirect, so the status codes Verification asks for (200 editor, 403
//! anonymous) are the page's own, not a redirect's.
//!
//! Add, edit, delete and bulk delete are dedicated routes rather than the
//! generic `/admin/content/{type}/add` and `/admin/content/{id}/edit`: those
//! hardcode `"create {type} content"` and a blanket `"edit any content"`
//! screen gate, which cannot express the flat `create media` / `edit own
//! media` / `edit any media` permissions this feature asks for. They still
//! reuse the same `item_form` pipeline and `ItemService` everything else
//! does — only the permission check at the door is different.

use std::collections::HashMap;

use axum::extract::{Multipart, Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::submission::PostedForm;
use crate::form::csrf::generate_csrf_token;
use crate::models::{CreateItem, Item, UpdateItem};
use crate::state::AppState;

use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    admin_user_context, render_admin_template, render_forbidden, render_not_found,
    render_server_error, require_csrf, require_csrf_header, require_login_json, require_permission,
};

/// Drupal's names: see `docs/BACKLOG.md` BL-115 and the PR description for
/// why these are flat (not per content type) — `trovato_media::tap_perm`
/// declares them, `trovato_media::tap_item_access` enforces the own/any
/// split per item.
pub const ACCESS_MEDIA_OVERVIEW: &str = "access media overview";
pub const CREATE_MEDIA: &str = "create media";

/// The library's sortable columns.
const MEDIA_SORTS: &[(&str, &str)] = &[("newest", "changed"), ("name", "lower(title)")];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/content/media", get(media_library))
        .route("/media", get(media_library))
        .route("/admin/content/media/add", get(choose_media_type))
        .route(
            "/admin/content/media/add/{media_type}",
            get(add_media_form).post(add_media_submit),
        )
        .route(
            "/admin/content/media/{id}/edit",
            get(edit_media_form).post(edit_media_submit),
        )
        .route("/admin/content/media/{id}/delete", post(delete_media))
        .route("/admin/content/media/bulk", post(bulk_media))
        .route("/admin/content/media/picker", get(picker_list))
        .route("/admin/content/media/picker/upload", post(picker_upload))
}

/// One media source as the templates and the picker show it: its content
/// type name and label.
#[derive(Debug, Clone, Serialize)]
struct MediaTypeOption {
    content_type: String,
    label: String,
}

async fn media_type_options(state: &AppState) -> Vec<MediaTypeOption> {
    state
        .media_sources()
        .sources()
        .iter()
        .map(|s| MediaTypeOption {
            content_type: s.content_type.clone(),
            label: s.label.clone(),
        })
        .collect()
}

/// The thumbnail URL for `item`, when it has a `field_file` naming an image;
/// `None` for a remote source or a non-image file (the template falls back to
/// a generic icon keyed on the MIME family).
async fn thumbnail_url(state: &AppState, item: &Item) -> Option<String> {
    if let Some(url) = file_field_thumbnail(state, &item.fields).await {
        return Some(url);
    }
    // A remote source has no file of its own; `field_thumbnail` names an
    // image media item to show instead (one level only — a thumbnail is not
    // itself expected to carry a thumbnail).
    let thumbnail_id = item.fields.get("field_thumbnail")?.as_str()?;
    let thumbnail_id: Uuid = thumbnail_id.parse().ok()?;
    let referenced = state.items().load(thumbnail_id).await.ok()??;
    file_field_thumbnail(state, &referenced.fields).await
}

/// The thumbnail URL for an image named by `fields["field_file"]`, when it is
/// an image; `None` for anything else, including a missing or non-image file.
async fn file_field_thumbnail(state: &AppState, fields: &serde_json::Value) -> Option<String> {
    let file_id = fields.get("field_file")?.as_str()?;
    let file_id: Uuid = file_id.parse().ok()?;
    let file = state.files().get(file_id).await.ok()??;
    if !file.filemime.starts_with("image/") {
        return None;
    }
    let path = file.uri.strip_prefix("local://").unwrap_or(&file.uri);
    Some(format!("/files/styles/thumbnail/{path}"))
}

/// `GET /admin/content/media` and `GET /media` (BL-115).
async fn media_library(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let user = match require_permission(&state, &session, ACCESS_MEDIA_OVERVIEW).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };

    let media_types: Vec<String> = state
        .media_sources()
        .content_types()
        .map(str::to_string)
        .collect();

    let type_filter = params
        .get("type")
        .map(String::as_str)
        .filter(|s| !s.is_empty() && state.media_sources().is_media_type(s));
    let author_filter = params.get("author").and_then(|s| Uuid::parse_str(s).ok());
    let view = if params.get("view").map(String::as_str) == Some("table") {
        "table"
    } else {
        "grid"
    };

    let query = ListQuery::from_params(&params);
    let sort = query.sort(MEDIA_SORTS, ("newest", true));
    let like = query.like();

    let types_to_query: Vec<String> = match type_filter {
        Some(t) => vec![t.to_string()],
        None => media_types.clone(),
    };

    let (items, total) = if types_to_query.is_empty() {
        (Vec::new(), 0)
    } else {
        match Item::media_page(
            state.db(),
            &types_to_query,
            like.as_deref(),
            author_filter,
            &sort.order_by(),
            PER_PAGE,
            query.offset(),
        )
        .await
        {
            Ok(page) => page,
            Err(e) => {
                tracing::error!(error = %e, "failed to list the media library");
                return render_server_error("Failed to load the media library.");
            }
        }
    };

    let mut authors: HashMap<String, String> = HashMap::new();
    let mut thumbnails: HashMap<String, String> = HashMap::new();
    let mut usage: HashMap<String, i64> = HashMap::new();
    for item in &items {
        if !authors.contains_key(&item.author_id.to_string())
            && let Ok(Some(user)) = state.users().find_by_id(item.author_id).await
        {
            authors.insert(item.author_id.to_string(), user.name);
        }
        if let Some(url) = thumbnail_url(&state, item).await {
            thumbnails.insert(item.id.to_string(), url);
        }
        let count = crate::content::media_refs::usage_count(state.db(), item.id)
            .await
            .unwrap_or(0);
        usage.insert(item.id.to_string(), count);
    }

    // Uploaders offered in the filter: only those who have authored media.
    let uploader_ids: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT author_id FROM item WHERE type = ANY($1) ORDER BY author_id",
    )
    .bind(&media_types)
    .fetch_all(state.db())
    .await
    .unwrap_or_default();
    let mut uploader_options: Vec<(String, String)> = Vec::new();
    for id in uploader_ids {
        if let Ok(Some(user)) = state.users().find_by_id(id).await {
            uploader_options.push((id.to_string(), user.name));
        }
    }

    let viewer = admin_user_context(&state, &user).await;
    let can_create = viewer.can(CREATE_MEDIA);
    let mut operations: HashMap<String, serde_json::Value> = HashMap::new();
    for item in &items {
        let edit = allowed(&state, item, "edit", &viewer).await;
        let delete = allowed(&state, item, "delete", &viewer).await;
        operations.insert(
            item.id.to_string(),
            serde_json::json!({ "edit": edit, "delete": delete }),
        );
    }

    let csrf_token = generate_csrf_token(&session).await;
    let mut type_options: Vec<(String, String)> = vec![];
    for source in state.media_sources().sources() {
        type_options.push((source.content_type.clone(), source.label.clone()));
    }

    let mut context = tera::Context::new();
    context.insert("items", &items);
    context.insert("operations", &operations);
    context.insert("authors", &authors);
    context.insert("thumbnails", &thumbnails);
    context.insert("usage", &usage);
    context.insert("type_options", &type_options);
    context.insert("type_filter", &type_filter.unwrap_or(""));
    context.insert("uploader_options", &uploader_options);
    context.insert(
        "author_filter",
        &author_filter.map(|a| a.to_string()).unwrap_or_default(),
    );
    context.insert("view", view);
    context.insert("can_create", &can_create);
    context.insert("list", &query.view(total, items.len(), Some(&sort)));
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/content/media");

    render_admin_template(
        &state,
        &super::helpers::get_user_context(&session, &state).await,
        "admin/media-library-list.html",
        context,
    )
    .await
}

async fn allowed(
    state: &AppState,
    item: &Item,
    op: &str,
    viewer: &crate::tap::UserContext,
) -> bool {
    matches!(state.items().check_access(item, op, viewer).await, Ok(true))
}

/// `GET /admin/content/media/add`: choose which media source to add.
async fn choose_media_type(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, CREATE_MEDIA).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let options = media_type_options(&state).await;
    let mut context = tera::Context::new();
    context.insert("media_types", &options);
    context.insert("path", "/admin/content/media/add");
    render_admin_template(
        &state,
        &admin_user_context(&state, &user).await,
        "admin/media-add-choose.html",
        context,
    )
    .await
}

async fn render_media_form(
    state: &AppState,
    session: &Session,
    user_ctx: &crate::tap::UserContext,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    item: Option<&Item>,
    posted: Option<&PostedForm>,
    errors: &[crate::form::ValidationError],
) -> Response {
    let path = match item {
        Some(item) => format!("/admin/content/media/{}/edit", item.id),
        None => format!("/admin/content/media/add/{}", content_type.machine_name),
    };
    let mut form = match super::item_form::build(
        state,
        session,
        user_ctx,
        content_type,
        item,
        &path,
        Vec::new(),
        false,
    )
    .await
    {
        Ok(form) => form,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the media form");
            return render_server_error("Failed to build the form.");
        }
    };
    if let Some(posted) = posted {
        super::item_form::redisplay(&mut form, posted);
    }
    let form_html = match state.theme().render_form_with_errors(&form, errors) {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to render the media form");
            return render_server_error("Failed to render the form.");
        }
    };

    let usage = match item {
        Some(item) => crate::content::media_refs::usage_count(state.db(), item.id)
            .await
            .unwrap_or(0),
        None => 0,
    };

    let mut context = tera::Context::new();
    context.insert("editing", &item.is_some());
    context.insert("content_type", content_type);
    context.insert("form_html", &form_html);
    context.insert("path", &path);
    context.insert("usage", &usage);
    if let Some(item) = item {
        context.insert("item_id", &item.id.to_string());
        let csrf_token = generate_csrf_token(session).await;
        context.insert("csrf_token", &csrf_token);
    }
    render_admin_template(state, user_ctx, "admin/media-form.html", context).await
}

fn requested_status(posted: &PostedForm) -> Option<i16> {
    posted
        .contains("_status_rendered")
        .then(|| i16::from(posted.contains("status")))
}

fn write_refusal(e: &anyhow::Error) -> Option<Vec<crate::form::ValidationError>> {
    if let Some(denied) = e.downcast_ref::<crate::content::WriteDenied>() {
        return Some(vec![crate::form::ValidationError::form(denied.to_string())]);
    }
    e.downcast_ref::<crate::content::InvalidFieldValues>()
        .map(|invalid| {
            invalid
                .0
                .iter()
                .map(|v| crate::form::ValidationError::field(&v.field, v.message.clone()))
                .collect()
        })
}

/// `GET /admin/content/media/add/{media_type}`.
async fn add_media_form(
    State(state): State<AppState>,
    session: Session,
    Path(media_type): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, CREATE_MEDIA).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if !state.media_sources().is_media_type(&media_type) {
        return render_not_found();
    }
    let Some(content_type) = state.content_types().get(&media_type) else {
        return render_not_found();
    };
    let user_ctx = admin_user_context(&state, &user).await;
    render_media_form(&state, &session, &user_ctx, &content_type, None, None, &[]).await
}

/// `POST /admin/content/media/add/{media_type}`.
async fn add_media_submit(
    State(state): State<AppState>,
    session: Session,
    Path(media_type): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, CREATE_MEDIA).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if !state.media_sources().is_media_type(&media_type) {
        return render_not_found();
    }
    let Some(content_type) = state.content_types().get(&media_type) else {
        return render_not_found();
    };
    let user_ctx = admin_user_context(&state, &user).await;

    let (form_state, fields_json) =
        match super::item_form::process(&state, &session, &user_ctx, &content_type, None, &posted)
            .await
        {
            Ok(processed) => processed,
            Err(crate::form::ProcessError::Csrf) => {
                return super::helpers::render_error(
                    "Invalid or expired form token. Please try again.",
                );
            }
            Err(crate::form::ProcessError::Invalid(errors)) => {
                return render_media_form(
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

    let input = CreateItem {
        item_type: media_type.clone(),
        title: posted.get("title").unwrap_or_default().trim().to_string(),
        author_id: user.id,
        status: requested_status(&posted).or(Some(1)),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        stage_id: None,
        language: None,
        log: Some("Added via the media library".to_string()),
    };

    match state.items().create(input, &user_ctx).await {
        Ok(item) => {
            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    Some(item.id),
                    &super::item_form::tap_state(&state, &user_ctx),
                )
                .await;
            Redirect::to("/admin/content/media").into_response()
        }
        Err(e) => {
            if let Some(errors) = write_refusal(&e) {
                return render_media_form(
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
            tracing::error!(error = %e, "failed to create media item");
            render_server_error("Failed to add this media item.")
        }
    }
}

/// `GET /admin/content/media/{id}/edit`.
async fn edit_media_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<Uuid>,
) -> Response {
    let user = match super::helpers::require_login(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };
    if !state.media_sources().is_media_type(&item.item_type) {
        return render_not_found();
    }
    let user_ctx = admin_user_context(&state, &user).await;
    match state.items().check_access(&item, "edit", &user_ctx).await {
        Ok(true) => {}
        Ok(false) => return render_forbidden("You may not edit this media item."),
        Err(e) => {
            tracing::error!(error = %e, "failed to check media edit access");
            return render_server_error("Failed to check access.");
        }
    }
    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };
    render_media_form(
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

/// `POST /admin/content/media/{id}/edit`.
async fn edit_media_submit(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match super::helpers::require_login(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };
    if !state.media_sources().is_media_type(&item.item_type) {
        return render_not_found();
    }
    let user_ctx = admin_user_context(&state, &user).await;
    match state.items().check_access(&item, "edit", &user_ctx).await {
        Ok(true) => {}
        Ok(false) => return render_forbidden("You may not edit this media item."),
        Err(e) => {
            tracing::error!(error = %e, "failed to check media edit access");
            return render_server_error("Failed to check access.");
        }
    }
    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };

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
            return super::helpers::render_error(
                "Invalid or expired form token. Please try again.",
            );
        }
        Err(crate::form::ProcessError::Invalid(errors)) => {
            return render_media_form(
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

    let input = UpdateItem {
        title: posted.get("title").map(|t| t.trim().to_string()),
        status: requested_status(&posted),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields_json)),
        log: Some("Updated via the media library".to_string()),
        stage_id: None,
        author_id: None,
        created: None,
    };

    match state.items().update(item_id, input, &user_ctx).await {
        Ok(_) => {
            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    Some(item_id),
                    &super::item_form::tap_state(&state, &user_ctx),
                )
                .await;
            Redirect::to("/admin/content/media").into_response()
        }
        Err(e) => {
            if let Some(errors) = write_refusal(&e) {
                return render_media_form(
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
            if e.to_string().contains("access denied") {
                return render_forbidden("You may not edit this media item.");
            }
            tracing::error!(error = %e, "failed to update media item");
            render_server_error("Failed to update this media item.")
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeleteMediaForm {
    #[serde(rename = "_token")]
    token: String,
    #[serde(default)]
    force: Option<String>,
}

/// `POST /admin/content/media/{id}/delete`.
///
/// Refuses the delete when the media item is still referenced by another
/// item (`crate::content::media_refs::usage_count`), unless `force` was
/// posted — the edit page shows a "Force delete" button only when there is a
/// use to force past, each going through the same `?confirm` flow every
/// other admin delete does.
async fn delete_media(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<Uuid>,
    Form(form): Form<DeleteMediaForm>,
) -> Response {
    let user = match super::helpers::require_login(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };
    if !state.media_sources().is_media_type(&item.item_type) {
        return render_not_found();
    }
    let user_ctx = admin_user_context(&state, &user).await;
    if !allowed(&state, &item, "delete", &user_ctx).await {
        return render_forbidden("You do not have permission to delete this media item.");
    }
    let force = form.force.as_deref() == Some("1");
    let usage = crate::content::media_refs::usage_count(state.db(), item_id)
        .await
        .unwrap_or(0);
    if usage > 0 && !force {
        return Redirect::to(&format!("/admin/content/media/{item_id}/edit")).into_response();
    }
    match state.items().delete(item_id, &user_ctx).await {
        Ok(true) => Redirect::to("/admin/content/media").into_response(),
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete media item");
            render_server_error("Failed to delete this media item.")
        }
    }
}

/// `POST /admin/content/media/bulk`: bulk delete only. Rows still in use are
/// skipped (not force-deleted in bulk — force is a deliberate, one-item
/// decision made from the edit page).
async fn bulk_media(
    State(state): State<AppState>,
    session: Session,
    body: axum::body::Bytes,
) -> Response {
    let user = match super::helpers::require_login(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let body = String::from_utf8_lossy(&body);
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let token = pairs
        .iter()
        .find(|(k, _)| k == "_token")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    if let Err(resp) = require_csrf(&session, token).await {
        return resp;
    }
    let ids: Vec<Uuid> = pairs
        .iter()
        .filter(|(k, _)| k == "ids[]")
        .filter_map(|(_, v)| v.parse().ok())
        .collect();
    let user_ctx = admin_user_context(&state, &user).await;
    for id in ids {
        let Some(item) = state.items().load(id).await.ok().flatten() else {
            continue;
        };
        if !state.media_sources().is_media_type(&item.item_type) {
            continue;
        }
        if !allowed(&state, &item, "delete", &user_ctx).await {
            continue;
        }
        let usage = crate::content::media_refs::usage_count(state.db(), id)
            .await
            .unwrap_or(0);
        if usage > 0 {
            continue;
        }
        if let Err(e) = state.items().delete(id, &user_ctx).await {
            tracing::warn!(item_id = %id, error = %e, "failed to bulk-delete a media item");
        }
    }
    Redirect::to("/admin/content/media").into_response()
}

// =============================================================================
// The picker dialog's JSON API
// =============================================================================

#[derive(Debug, Serialize)]
struct PickerItem {
    id: String,
    label: String,
    #[serde(rename = "type")]
    content_type: String,
    type_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnail_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct PickerResponse {
    items: Vec<PickerItem>,
}

const PICKER_PAGE_SIZE: i64 = 24;

/// `GET /admin/content/media/picker`: the dialog's own listing, same
/// permission as the library page it is drawn from.
async fn picker_list(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(refusal) = require_permission(&state, &session, ACCESS_MEDIA_OVERVIEW).await {
        return refusal;
    }
    let media_types: Vec<String> = state
        .media_sources()
        .content_types()
        .map(str::to_string)
        .collect();
    let type_filter = params
        .get("type")
        .map(String::as_str)
        .filter(|s| !s.is_empty() && state.media_sources().is_media_type(s));
    let types_to_query: Vec<String> = match type_filter {
        Some(t) => vec![t.to_string()],
        None => media_types,
    };
    let q = params
        .get("q")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let like = q.map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .filter(|p| *p >= 1)
        .unwrap_or(1);

    if types_to_query.is_empty() {
        return Json(PickerResponse { items: Vec::new() }).into_response();
    }
    let (rows, _total) = match Item::media_page(
        state.db(),
        &types_to_query,
        like.as_deref(),
        None,
        "changed DESC",
        PICKER_PAGE_SIZE,
        (page - 1) * PICKER_PAGE_SIZE,
    )
    .await
    {
        Ok(page) => page,
        Err(e) => {
            tracing::error!(error = %e, "failed to list media for the picker");
            return render_server_error("Failed to load media.").into_response();
        }
    };

    let mut items = Vec::with_capacity(rows.len());
    for item in &rows {
        let type_label = state
            .media_sources()
            .get(&item.item_type)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| item.item_type.clone());
        items.push(PickerItem {
            id: item.id.to_string(),
            label: item.title.clone(),
            content_type: item.item_type.clone(),
            type_label,
            thumbnail_url: thumbnail_url(&state, item).await,
        });
    }
    Json(PickerResponse { items }).into_response()
}

#[derive(Debug, Serialize)]
struct PickerUploadResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<PickerItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// `POST /admin/content/media/picker/upload`: create a new media item from
/// the dialog's "Upload new media" section, in one request. `media_type`
/// names one of the registered sources; a file-backed source (`media_file`,
/// `media_image`) reads the `file` part, a URL-backed source (anything else
/// registered) reads `remote_url`.
async fn picker_upload(
    State(state): State<AppState>,
    session: Session,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let user = match require_login_json(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal.into_response(),
    };
    if let Err(refusal) = require_csrf_header(&session, &headers).await {
        return refusal.into_response();
    }

    let mut media_type = String::new();
    let mut title = String::new();
    let mut remote_url = String::new();
    let mut file_part: Option<(String, Option<String>, Vec<u8>)> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name().unwrap_or_default() {
            "media_type" => media_type = field.text().await.unwrap_or_default(),
            "title" => title = field.text().await.unwrap_or_default(),
            "remote_url" => remote_url = field.text().await.unwrap_or_default(),
            "file" => {
                let filename = field.file_name().map(str::to_string);
                let content_type = field.content_type().map(str::to_string);
                if let Ok(data) = field.bytes().await {
                    file_part = Some((filename.unwrap_or_default(), content_type, data.to_vec()));
                }
            }
            _ => {}
        }
    }

    if !state.media_sources().is_media_type(&media_type) {
        return Json(PickerUploadResponse {
            success: false,
            item: None,
            error: Some("Unknown media type.".to_string()),
        })
        .into_response();
    }
    let Some(content_type) = state.content_types().get(&media_type) else {
        return Json(PickerUploadResponse {
            success: false,
            item: None,
            error: Some("Unknown media type.".to_string()),
        })
        .into_response();
    };

    let is_remote = media_type == "media_remote_video" || media_type == "media_remote_embed";
    let mut fields = serde_json::Map::new();
    let fallback_title;
    if is_remote {
        if remote_url.trim().is_empty() {
            return Json(PickerUploadResponse {
                success: false,
                item: None,
                error: Some("A URL is required.".to_string()),
            })
            .into_response();
        }
        fields.insert(
            "field_remote_url".to_string(),
            serde_json::Value::String(remote_url.trim().to_string()),
        );
        fallback_title = remote_url.trim().to_string();
    } else {
        let Some((filename, content_type_hint, data)) = file_part else {
            return Json(PickerUploadResponse {
                success: false,
                item: None,
                error: Some("A file is required.".to_string()),
            })
            .into_response();
        };
        let mime_type = crate::file::resolve_mime_type(content_type_hint.as_deref(), &filename);
        let uploaded = match state
            .files()
            .upload(user.id, &filename, &mime_type, &data)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                return Json(PickerUploadResponse {
                    success: false,
                    item: None,
                    error: Some(e.to_string()),
                })
                .into_response();
            }
        };
        fields.insert(
            "field_file".to_string(),
            serde_json::Value::String(uploaded.id.to_string()),
        );
        fallback_title = filename;
    }

    let input = CreateItem {
        item_type: media_type.clone(),
        title: if title.trim().is_empty() {
            fallback_title
        } else {
            title.trim().to_string()
        },
        author_id: user.id,
        status: Some(1),
        promote: None,
        sticky: None,
        fields: Some(serde_json::Value::Object(fields)),
        stage_id: None,
        language: None,
        log: Some("Added via the media picker".to_string()),
    };
    let user_ctx = admin_user_context(&state, &user).await;
    match state.items().create(input, &user_ctx).await {
        Ok(item) => {
            let type_label = content_type.label.clone();
            Json(PickerUploadResponse {
                success: true,
                item: Some(PickerItem {
                    id: item.id.to_string(),
                    label: item.title.clone(),
                    content_type: item.item_type.clone(),
                    type_label,
                    thumbnail_url: thumbnail_url(&state, &item).await,
                }),
                error: None,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create media item from the picker");
            Json(PickerUploadResponse {
                success: false,
                item: None,
                error: Some("Failed to add this media item.".to_string()),
            })
            .into_response()
        }
    }
}
