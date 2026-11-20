//! Item CRUD route handlers.
//!
//! Provides endpoints for viewing, creating, editing, and deleting content items.

use axum::{
    Extension, Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::{FieldMode, FilterPipeline, ItemRevisions, RevisionAccess, RevisionSelection};
use crate::error::AppError;
use crate::form::csrf::generate_csrf_token;
use crate::middleware::language::ResolvedLanguage;
use crate::models::{CreateItem, UpdateItem, UrlAlias};
use crate::state::AppState;
use crate::tap::UserContext;

use super::helpers::{CsrfOnlyForm, JsonError, html_escape};

/// Response for successful item operations.
#[derive(Debug, Serialize)]
pub struct ItemResponse {
    pub id: Uuid,
    pub title: String,
    pub item_type: String,
    pub status: i16,
}

/// Full item response for JSON API.
#[derive(Debug, Serialize)]
pub struct ItemApiResponse {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub item_type: String,
    pub title: String,
    pub status: i16,
    pub author_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<AuthorResponse>,
    pub created: i64,
    pub changed: i64,
    pub promote: i16,
    pub sticky: i16,
    pub fields: serde_json::Value,
    pub stage_id: Uuid,
    /// Links copies of the same logical item across stages.
    pub item_group_id: Uuid,
}

/// Author information for embedding.
#[derive(Debug, Clone, Serialize)]
pub struct AuthorResponse {
    pub id: Uuid,
    pub name: String,
}

/// Paginated list response.
#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub pagination: PaginationMeta,
}

/// Pagination metadata.
#[derive(Debug, Serialize)]
pub struct PaginationMeta {
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

/// Query parameters for listing items.
#[derive(Debug, Deserialize)]
pub struct ListItemsQuery {
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    pub status: Option<i16>,
    pub author_id: Option<Uuid>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub include: Option<String>,
}

/// Query parameters for an item page.
#[derive(Debug, Default, Deserialize)]
pub struct ViewItemQuery {
    /// Outcome of a comment just posted from the page's own form: `posted`,
    /// `pending` or `error`. Set by the redirect the comment route issues for a
    /// form submission, so a reader without JavaScript is told what happened —
    /// a held comment would otherwise appear to have vanished.
    pub comment: Option<String>,
}

/// Query parameters for getting a single item.
#[derive(Debug, Deserialize)]
pub struct GetItemQuery {
    pub include: Option<String>,
}

/// Request for creating an item.
#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub title: String,
    pub status: Option<i16>,
    pub fields: Option<serde_json::Value>,
    pub log: Option<String>,
}

/// Request for updating an item.
#[derive(Debug, Deserialize)]
pub struct UpdateItemRequest {
    pub title: Option<String>,
    pub status: Option<i16>,
    pub fields: Option<serde_json::Value>,
    pub log: Option<String>,
    pub url_alias: Option<String>,
}

// =============================================================================
// G-ITEM-FORM-MISMATCH — one submission shape for two request encodings
// =============================================================================

/// The hidden input `FormBuilder` emits beside the published checkbox.
///
/// `_`-prefixed, so it is form machinery and never stored as a field.
const STATUS_RENDERED_KEY: &str = "_status_rendered";

/// One item create/update submission, however it arrived.
///
/// `GET /item/add/{type}` renders an HTML `<form>`, and an HTML form posts
/// `application/x-www-form-urlencoded` — but the matching `POST` extracted
/// `Json<CreateItemRequest>`, so **submitting the page the kernel had just
/// rendered was a 415 by construction** (`G-ITEM-FORM-MISMATCH`, Argus M3). The
/// edit pair had the same defect, and its URL-alias input was concatenated
/// *after* `</form>`, so it was never submitted either.
///
/// Rather than delete the form or break the JSON API, the route now accepts
/// both encodings and normalizes them here. JSON requests are byte-for-byte
/// unchanged, including where their CSRF token comes from:
///
/// - `application/json` → the existing request body; CSRF from the
///   `X-CSRF-Token` header.
/// - `application/x-www-form-urlencoded` → the forms pipeline: CSRF from the
///   `_token` hidden input, because an HTML form cannot set a header, checked
///   by `FormService::process` with the form state.
///
/// A form body's field values are not read here: they go to
/// [`crate::content::submission::build_item_fields`] with the form state's
/// rendered list, so a save writes what the form showed and nothing else.
#[derive(Debug, Default)]
pub struct ItemSubmission {
    /// New title. `None` on an update means "leave it".
    pub title: Option<String>,
    /// Published flag.
    pub status: Option<i16>,
    /// Dynamic content fields, from a JSON body. A form body leaves this
    /// `None` and carries [`Self::posted`] instead.
    pub fields: Option<serde_json::Value>,
    /// Every pair of a form body, for the forms pipeline; `None` on JSON.
    pub posted: Option<crate::content::submission::PostedForm>,
    /// Revision log message.
    pub log: Option<String>,
    /// URL alias, update path only.
    pub url_alias: Option<String>,
    /// The stage asked for, as sent: `_stage` on a form, `stage_id` in JSON.
    /// Read through `ItemSubmission::requested_stage`.
    pub stage: Option<String>,
}

impl ItemSubmission {
    /// The stage this submission asks for: `None` when it asked for none (or
    /// sent an empty value, the selector's "stay"), a 400 when the value is not
    /// a stage id. Whether the item may go there is `ItemService`'s decision.
    fn requested_stage(&self) -> Result<Option<Uuid>, AppError> {
        match self.stage.as_deref().map(str::trim) {
            None | Some("") => Ok(None),
            Some(value) => Uuid::parse_str(value)
                .map(Some)
                .map_err(|_| AppError::bad_request("stage must be a stage id")),
        }
    }

    /// Build a submission from a decoded urlencoded form body.
    fn from_form(pairs: Vec<(String, String)>) -> Self {
        let form = crate::content::submission::PostedForm(pairs);
        let get = |name: &str| form.get(name).map(str::to_string);

        Self {
            title: get("title"),
            // A checkbox posts nothing when unchecked, so an absent `status`
            // means "unpublished" — but only if the control was on the page at
            // all. The form renders it only for a user who may publish, and
            // stamps `_status_rendered` beside it; without that marker the
            // submission says nothing about status rather than silently
            // unpublishing the item.
            status: form
                .contains(STATUS_RENDERED_KEY)
                .then(|| i16::from(form.contains("status"))),
            fields: None,
            log: get("log")
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty()),
            url_alias: get("url_alias"),
            stage: get("_stage"),
            posted: Some(form),
        }
    }

    /// Build a submission from a decoded JSON body.
    fn from_json(body: serde_json::Value) -> Self {
        Self {
            title: body.get("title").and_then(|v| v.as_str()).map(String::from),
            status: body
                .get("status")
                .and_then(serde_json::Value::as_i64)
                .map(|n| n as i16),
            fields: body.get("fields").cloned(),
            log: body.get("log").and_then(|v| v.as_str()).map(String::from),
            url_alias: body
                .get("url_alias")
                .and_then(|v| v.as_str())
                .map(String::from),
            stage: body
                .get("stage_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            posted: None,
        }
    }
}

impl<S: Send + Sync> axum::extract::FromRequest<S> for ItemSubmission {
    type Rejection = AppError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        if content_type.starts_with("application/x-www-form-urlencoded") {
            let Form(form) = Form::<Vec<(String, String)>>::from_request(req, state)
                .await
                .map_err(|e| AppError::bad_request(format!("invalid form body: {e}")))?;
            return Ok(Self::from_form(form));
        }

        // Everything else keeps the pre-existing JSON behaviour, including the
        // 415 an unsupported content type already produced.
        let Json(body) = Json::<serde_json::Value>::from_request(req, state)
            .await
            .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))?;
        Ok(Self::from_json(body))
    }
}

/// Map a service error to its response, turning a write the gates refused into
/// a 403 that names what was refused.
///
/// `WriteDenied` is the typed error both item write gates return
/// (`ItemService::gate_field_writes` and `ItemService::gate_publish`); the
/// field names it carries are already public through `/api/content-types`, so
/// naming them is what lets a client fix its request. Everything else keeps the
/// behaviour it had: the item-level `access denied` bail is a 403, and an
/// unrecognized error is a 500 with `context`.
fn write_error(e: anyhow::Error, context: &'static str) -> AppError {
    if let Some(denied) = e.downcast_ref::<crate::content::WriteDenied>() {
        return AppError::forbidden(denied.to_string());
    }
    if e.to_string().contains("access denied") {
        return AppError::forbidden("Access denied");
    }
    AppError::internal_ctx(e, context)
}

/// Create the item router.
pub fn router() -> Router<AppState> {
    Router::new()
        // View item
        .route("/item/{id}", get(view_item))
        // Add item form and submission
        .route("/item/add/{type}", get(add_item_form))
        .route("/item/add/{type}", post(create_item))
        // Edit item form and submission
        .route("/item/{id}/edit", get(edit_item_form))
        .route("/item/{id}/edit", post(update_item))
        // Delete item
        .route("/item/{id}/delete", post(delete_item))
        // Revision history
        .route("/item/{id}/revisions", get(list_revisions))
        .route("/item/{id}/revisions/compare", get(compare_revisions))
        .route("/item/{id}/revisions/{rev_id}", get(preview_revision))
        .route("/item/{id}/revert/{rev_id}", post(revert_revision))
        // API endpoints
        .route("/api/content-types", get(list_content_types))
        .route("/api/items/{type}", get(list_items_by_type))
        // JSON API endpoints
        .route("/api/item/{id}", get(get_item_api))
        .route("/api/items", get(list_items_api))
}

// The session-derived loader lives in `routes::helpers` alongside its sibling
// for handlers that already hold a `User`, so there is one implementation of
// "the viewer's real permissions" rather than one per module. Re-exported here
// because the read-path routes (comments, gather, search, file, batch, api_v1)
// reach it as `routes::item::get_user_context`.
pub(crate) use super::helpers::get_user_context;

/// Determine which text formats the user is allowed to use.
///
/// Admins get all formats. Other users get formats based on their
/// `"use filtered_html"` and `"use full_html"` permissions.
/// `plain_text` is always allowed (handled by FormBuilder).
/// The render mode for every field of `content_type`, for this viewer.
///
/// Two batched field-access decisions — `"view"` and `"edit"` — one dispatch
/// each, and [`FieldMode::from_decisions`] turns the pair into a mode. The edit
/// form used to render every field of the type with its stored value, which
/// showed a viewer the contents of fields the read paths were careful to drop.
pub(crate) async fn field_modes_for(
    state: &AppState,
    user: &UserContext,
    item_type: &str,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    creating: bool,
) -> std::collections::HashMap<String, FieldMode> {
    let names: Vec<String> = content_type
        .fields
        .iter()
        .map(|f| f.field_name.clone())
        .collect();
    let may_view = state
        .items()
        .field_access_decisions(user, item_type, &names, "view")
        .await;
    let may_edit = state
        .items()
        .field_access_decisions(user, item_type, &names, "edit")
        .await;
    names
        .into_iter()
        .map(|name| {
            let mode = FieldMode::from_decisions(
                may_view.get(&name).copied().unwrap_or(true),
                may_edit.get(&name).copied().unwrap_or(true),
                creating,
            );
            (name, mode)
        })
        .collect()
}

/// View an item.
async fn view_item(
    State(state): State<AppState>,
    Extension(lang): Extension<ResolvedLanguage>,
    requested: Option<Extension<crate::middleware::RequestedPath>>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(query): Query<ViewItemQuery>,
) -> Result<Html<String>, (StatusCode, Json<JsonError>)> {
    let user = get_user_context(&session, &state).await;

    // Load item with view tap invocation
    let (mut item, render_outputs) = match state.items().load_for_view(id, &user).await {
        Ok(Some(data)) => data,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(JsonError {
                    error: "Item not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load item");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(JsonError {
                    error: "Internal server error".to_string(),
                }),
            ));
        }
    };

    // Overlay translation if the active language differs from the default
    let active_language = lang.0;
    if active_language != state.default_language() {
        super::helpers::apply_translation_overlay(state.items(), &mut item, &active_language).await;

        // The overlay may re-materialize a field `load_for_view` dropped (a
        // translation can carry a restricted field's value). Re-apply the
        // field-access filter so the SSR output never leaks it (FR-8 Story 3.6).
        let names: Vec<String> = item
            .fields
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        if !names.is_empty() {
            let allowed: std::collections::HashSet<String> = state
                .items()
                .accessible_fields(&user, &item.item_type, &names, "view")
                .await
                .into_iter()
                .collect();
            if let Some(obj) = item.fields.as_object_mut() {
                obj.retain(|k, _| allowed.contains(k));
            }
        }
    }

    // Look up content type field definitions for Blocks detection
    let content_type_fields = state
        .content_types()
        .get(&item.item_type)
        .map(|ct| ct.fields.clone())
        .unwrap_or_default();

    // Render fields through filter pipeline
    let mut children_html = render_item_fields(&state, &item.fields, &content_type_fields);

    // Include plugin render outputs
    for output in render_outputs {
        children_html.push_str(&output);
    }

    // Resolve forward RecordReference fields to linked item titles/URLs
    let mut referenced_items: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    if let Some(ct) = state.content_types().get(&item.item_type) {
        for field_def in &ct.fields {
            if matches!(
                field_def.field_type,
                trovato_sdk::types::FieldType::RecordReference(_)
            ) && let Some(val) = item.fields.get(&field_def.field_name)
            {
                let ids: Vec<&str> = if let Some(s) = val.as_str() {
                    vec![s]
                } else if let Some(arr) = val.as_array() {
                    arr.iter().filter_map(|v| v.as_str()).collect()
                } else {
                    vec![]
                };
                let mut refs = Vec::new();
                for id_str in ids {
                    // The viewer passed `check_access` for *this* item, which
                    // says nothing about the items it points at: a reference to
                    // a draft used to render that draft's title to anyone who
                    // could see the referring page.
                    if let Ok(ref_id) = id_str.parse::<Uuid>()
                        && let Ok(Some(ref_item)) = state.items().load(ref_id).await
                        && state
                            .items()
                            .check_access(&ref_item, "view", &user)
                            .await
                            .unwrap_or(false)
                    {
                        refs.push(serde_json::json!({
                            "id": ref_item.id,
                            "title": ref_item.title,
                            "type": ref_item.item_type,
                        }));
                    }
                }
                if !refs.is_empty() {
                    referenced_items.insert(field_def.field_name.clone(), refs);
                }
            }
        }
    }

    // Resolve File fields to the files this viewer may fetch: one query for
    // every file the item names, filtered by the rule `/files/{path}` applies.
    let referenced_files = state
        .items()
        .referenced_files(
            &item,
            &content_type_fields,
            |uri| state.files().storage().public_url(uri),
            &user,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, item_id = %item.id, "failed to resolve file fields");
            std::collections::HashMap::new()
        });

    // Resolve reverse references: items that reference this item (P11g / D-57).
    //
    // For each content type's RecordReference field, a GIN-indexed JSONB
    // containment query (`fields @> '{"field": "<uuid>"}'` / `@> '{"field":
    // ["<uuid>"]}'`) returns *every* referring item of that type — superset-
    // correct, replacing the former `LIMIT 50`-per-type scan that silently
    // dropped referrers past the fiftieth. Results stay grouped by the declaring
    // content type, matching the prior output shape.
    let mut reverse_references: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for ct in state.content_types().list_all().await {
        for field_def in &ct.fields {
            if !matches!(
                field_def.field_type,
                trovato_sdk::types::FieldType::RecordReference(_)
            ) {
                continue;
            }
            if let Ok(found) = state
                .items()
                .find_referencing(Some(&ct.machine_name), &field_def.field_name, item.id)
                .await
            {
                // Every referrer, through the shared seam: a draft pointing at
                // a published item used to name itself on that item's page.
                // `found.len()` as the page size keeps the superset-correct
                // shape — the filter is an access decision, not a cap.
                let page_size = found.len();
                let found = state
                    .items()
                    .filter_page_for_view(found, &user, "view", page_size)
                    .await;
                for found_item in found {
                    reverse_references
                        .entry(ct.machine_name.clone())
                        .or_default()
                        .push(serde_json::json!({
                            "id": found_item.id,
                            "title": found_item.title,
                            "type": found_item.item_type,
                        }));
                }
            }
        }
    }

    // Resolve item template via theme engine
    let suggestions = [
        format!("elements/item--{}--{}", item.item_type, item.id),
        format!("elements/item--{}", item.item_type),
        "elements/item".to_string(),
    ];
    let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();
    let template = state
        .theme()
        .resolve_template(&suggestion_refs)
        .unwrap_or_else(|| "elements/item.html".to_string());

    // Build safe_urls map: only http/https URLs from item fields, keyed by field name.
    // Prevents javascript: URI injection in template href attributes where Tera
    // autoescape does not protect (it escapes HTML entities, not URI schemes).
    let mut safe_urls: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(fields) = item.fields.as_object() {
        for (key, val) in fields {
            if let Some(url) = val.as_str()
                && (url.starts_with("http://") || url.starts_with("https://"))
            {
                safe_urls.insert(key.clone(), url.to_string());
            }
        }
    }

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("children", &children_html);
    context.insert("referenced_items", &referenced_items);
    context.insert("referenced_files", &referenced_files);
    context.insert("reverse_references", &reverse_references);
    context.insert("safe_urls", &safe_urls);
    context.insert("active_language", &active_language);
    context.insert(
        "text_direction",
        crate::middleware::language::text_direction_for_language(&active_language),
    );

    let item_html = state
        .theme()
        .tera()
        .render(&template, &context)
        .unwrap_or_else(|_| {
            // Fallback if template rendering fails
            format!("<h1>{}</h1>{}", html_escape(&item.title), children_html)
        });

    // Wrap in page layout with site context.
    //
    // `requested_path` goes in first: this route is the common case for reaching
    // a page through an alias, so `current_path` here is `/item/{uuid}` and the
    // address the visitor typed is the only thing a menu can be matched against.
    let item_path = format!("/item/{id}");
    if let Some(Extension(crate::middleware::RequestedPath(ref requested_path))) = requested {
        context.insert("requested_path", requested_path);
    }
    super::helpers::inject_site_context(&state, &session, &mut context, &item_path).await;

    // Build breadcrumbs: Home > Content Type Label > Item Title
    let type_label = state
        .content_types()
        .get(&item.item_type)
        .map(|ct| ct.label.clone())
        .unwrap_or_else(|| item.item_type.clone());
    let breadcrumbs = vec![
        serde_json::json!({"path": "/", "title": "Home"}),
        serde_json::json!({"path": null, "title": type_label}),
        serde_json::json!({"path": null, "title": item.title}),
    ];
    context.insert("breadcrumbs", &breadcrumbs);

    // Head metadata: meta description, canonical link, Open Graph tags. Built
    // here rather than in a plugin because `<head>` is not reachable from one —
    // `tap_item_view` output is appended to the item's body. The canonical URL
    // is the item's alias when it has one, so the address a crawler indexes is
    // the address the site links to.
    let canonical_path = UrlAlias::get_canonical_alias_with_context(
        state.db(),
        &item_path,
        item.stage_id,
        &active_language,
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| item_path.clone());
    let site_name = context
        .get("site_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Trovato")
        .to_string();
    let page_meta = crate::content::PageMeta::for_item(
        &item,
        &canonical_path,
        &state.runtime().site_url,
        &site_name,
        &content_type_fields,
    );
    context.insert("page_meta", &page_meta);

    // Every language this item can be read in, and where. A theme builds a
    // language switcher out of `available_translations`; `hreflang_links` is the
    // same facts for a crawler, and is left out entirely when there is only one
    // language to name, since a lone self-referential alternate says nothing.
    let translations = super::helpers::available_translations(&state, id, &canonical_path).await;
    if translations.len() > 1 {
        context.insert(
            "hreflang_links",
            &super::helpers::build_hreflang_links(&translations, state.default_language()),
        );
    }
    context.insert("available_translations", &translations);

    // The AI Assistant's launcher, for every scope that names this item's type.
    //
    // The kernel adds it rather than the plugin, because a plugin's `tap_item_view`
    // output is appended to the body and a plugin cannot know whether the viewer
    // may open the conversation. The partial renders nothing when the assistant
    // is off; `inject_site_context` above already put that flag in the context.
    let launchers = render_item_launchers(&state, &context, &item.item_type, id);
    let item_html = format!("{launchers}{item_html}");

    // The comment thread, under the item. Empty when comments are disabled.
    let comments_html = super::comment::render_thread(
        &state,
        &session,
        &item,
        &user,
        &canonical_path,
        query.comment.as_deref(),
    )
    .await;
    let item_html = format!("{item_html}{comments_html}");

    let page_html = state
        .theme()
        .render_page(&item_path, &item.title, &item_html, &mut context)
        .unwrap_or_else(|_| format!("<!DOCTYPE html><html><body>{item_html}</body></html>"));

    Ok(Html(page_html))
}

/// Render an item's fields as its page shows them: Blocks, page builder,
/// compound sections, formatted text and plain values, each through the
/// filter pipeline.
///
/// One body for the item page and the revision preview, so a past revision is
/// shown exactly as it would have been. The caller hands in fields already cut
/// to what the viewer may see.
fn render_item_fields(
    state: &AppState,
    fields: &serde_json::Value,
    content_type_fields: &[trovato_sdk::types::FieldDefinition],
) -> String {
    let mut children_html = String::new();
    if let Some(fields) = fields.as_object() {
        for (name, value) in fields {
            // Blocks field: flat JSON array of {type, weight, data}
            let is_blocks_field = content_type_fields.iter().any(|f| {
                f.field_name == *name
                    && matches!(f.field_type, trovato_sdk::types::FieldType::Blocks)
            });
            if is_blocks_field {
                if let Some(blocks) = value.as_array() {
                    let rendered = crate::content::render_blocks(blocks);
                    children_html.push_str(&format!(
                        "<div class=\"field field--blocks field-{}\">{}</div>",
                        html_escape(name),
                        rendered
                    ));
                }
                continue;
            }

            // PageBuilder field: Puck JSON component tree.
            // Detect via field type definition OR structural check (root+content keys).
            let is_page_builder_field = content_type_fields.iter().any(|f| {
                f.field_name == *name
                    && matches!(f.field_type, trovato_sdk::types::FieldType::PageBuilder)
            }) || (value.get("root").is_some()
                && value.get("content").is_some());
            if is_page_builder_field {
                match state.theme().render_page_builder_content(value) {
                    Ok(rendered) => {
                        children_html.push_str(&format!(
                            "<div class=\"field field--page-builder field-{}\">{}</div>",
                            html_escape(name),
                            rendered
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(field = %name, error = %e, "failed to render page builder field");
                    }
                }
                continue;
            }

            // Compound field: has "sections" array
            if let Some(sections_raw) = value.get("sections").and_then(|s| s.as_array()) {
                // Sort sections by weight for correct display order
                let mut sorted_sections = sections_raw.clone();
                sorted_sections
                    .sort_by_key(|s| s.get("weight").and_then(|w| w.as_i64()).unwrap_or(0));

                for section in &sorted_sections {
                    let section_type = section
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown");

                    // Sanitize section_type for template suggestion: only allow
                    // alphanumeric, hyphens, and underscores to prevent path traversal
                    let safe_type: String = section_type
                        .chars()
                        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                        .collect();

                    // Process section data fields through FilterPipeline
                    let mut section_fields_html = String::new();
                    if let Some(data) = section.get("data").and_then(|d| d.as_object()) {
                        for (_key, val) in data {
                            if let (Some(text), Some(fmt)) = (
                                val.get("value").and_then(|v| v.as_str()),
                                val.get("format").and_then(|v| v.as_str()),
                            ) {
                                let filtered = FilterPipeline::for_format_safe(fmt).process(text);
                                section_fields_html.push_str(&filtered);
                            } else if let Some(text) = val.as_str() {
                                let filtered =
                                    FilterPipeline::for_format("plain_text").process(text);
                                section_fields_html.push_str(&filtered);
                            } else {
                                // Render non-string values (Integer, Float, Boolean) as
                                // escaped text so they're not silently dropped
                                if !val.is_object() && !val.is_array() && !val.is_null() {
                                    let text = val.to_string();
                                    let filtered =
                                        FilterPipeline::for_format("plain_text").process(&text);
                                    section_fields_html.push_str(&filtered);
                                }
                            }
                        }
                    }

                    // Try to resolve section template using sanitized type
                    let suggestions = [
                        format!("elements/compound-section--{safe_type}"),
                        "elements/compound-section".to_string(),
                    ];
                    let suggestion_refs: Vec<&str> =
                        suggestions.iter().map(|s| s.as_str()).collect();
                    let template = state
                        .theme()
                        .resolve_template(&suggestion_refs)
                        .unwrap_or_else(|| "elements/compound-section.html".to_string());

                    // Build sanitized section data: HTML-escape all string values
                    // so custom templates can safely use {{ section_data.field }}
                    let sanitized_data = if let Some(data) =
                        section.get("data").and_then(|d| d.as_object())
                    {
                        let mut clean = serde_json::Map::new();
                        for (k, v) in data {
                            if let Some(s) = v.as_str() {
                                clean.insert(k.clone(), serde_json::json!(html_escape(s)));
                            } else if let Some(obj) = v.as_object() {
                                // Escape string values inside nested objects like {value, format}
                                let mut inner = serde_json::Map::new();
                                for (ik, iv) in obj {
                                    if let Some(s) = iv.as_str() {
                                        inner.insert(ik.clone(), serde_json::json!(html_escape(s)));
                                    } else {
                                        inner.insert(ik.clone(), iv.clone());
                                    }
                                }
                                clean.insert(k.clone(), serde_json::Value::Object(inner));
                            } else {
                                clean.insert(k.clone(), v.clone());
                            }
                        }
                        serde_json::Value::Object(clean)
                    } else {
                        serde_json::json!({})
                    };

                    let mut section_ctx = tera::Context::new();
                    section_ctx.insert("section_data", &sanitized_data);
                    section_ctx.insert("section_type", &safe_type);
                    section_ctx.insert("section_body", &section_fields_html);

                    let section_html = state
                        .theme()
                        .tera()
                        .render(&template, &section_ctx)
                        .unwrap_or_else(|_| {
                            format!(
                                "<div class=\"compound-section compound-section--{}\">{}</div>",
                                html_escape(&safe_type),
                                section_fields_html
                            )
                        });
                    children_html.push_str(&section_html);
                }
            } else if let Some(text_val) = value.get("value").and_then(|v| v.as_str()) {
                let raw_fmt = value
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plain_text");
                let filtered = FilterPipeline::for_format_safe(raw_fmt).process(text_val);
                children_html.push_str(&format!(
                    "<div class=\"field field-{}\">{}</div>",
                    html_escape(name),
                    filtered
                ));
            } else if !value.is_object() && !value.is_array() && !value.is_null() {
                // Plain string, numeric or boolean field (e.g. "field_city":
                // "Portland"). The label is the one the item type declares,
                // else derived from the machine name. The separator sits inside
                // the label element, so a theme that hides the label hides it
                // too instead of leaving a bare ": true".
                let label =
                    crate::content::revision_compare::field_label(content_type_fields, name);
                let text = match value.as_str() {
                    Some(s) => s.to_string(),
                    None => value.to_string(),
                };
                let filtered = FilterPipeline::for_format("plain_text").process(&text);
                children_html.push_str(&format!(
                    "<div class=\"field field-{}\"><strong class=\"field__label\">{}:</strong> {}</div>",
                    html_escape(name),
                    html_escape(&label),
                    filtered
                ));
            }
        }
    }
    children_html
}

/// Render the assistant launcher for every scope that applies to this item type.
///
/// Returns an empty string when no scope claims the type, which is the common
/// case: the launcher is opt-in per content type, declared by the plugin that
/// owns the scope.
fn render_item_launchers(
    state: &AppState,
    page_context: &tera::Context,
    item_type: &str,
    item_id: Uuid,
) -> String {
    let scopes = state.assistant_scopes().scopes_for_item_type(item_type);
    if scopes.is_empty() {
        return String::new();
    }

    let mut html = String::new();
    for registered in scopes {
        let mut context = tera::Context::new();
        context.insert(
            "assistant_enabled",
            &page_context
                .get("assistant_enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        );
        // Only what the viewer may open: `assistant_scopes` is already filtered
        // to this viewer's permissions.
        let permitted = page_context
            .get("assistant_scopes")
            .and_then(|value| value.as_object())
            .is_some_and(|scopes| scopes.contains_key(&registered.scope.name));
        if !permitted {
            continue;
        }
        context.insert("scope", &registered.scope.name);
        context.insert("scope_id", &item_id.to_string());
        context.insert(
            "assistant_label",
            &format!("Configure with AI: {}", registered.scope.label),
        );
        if let Ok(rendered) = state
            .theme()
            .tera()
            .render("assistant/launcher.html", &context)
        {
            html.push_str(rendered.trim());
        }
    }
    html
}

/// Display add item form.
async fn add_item_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_type): Path<String>,
) -> Result<Response, AppError> {
    let user = get_user_context(&session, &state).await;

    let permission = format!("create {item_type} content");
    if !user.can(&permission) {
        return Err(AppError::forbidden("Access denied"));
    }
    let content_type = state
        .content_types()
        .get(&item_type)
        .ok_or_else(|| AppError::not_found("content type"))?;

    item_form_page(&state, &session, &user, &content_type, None, None, &[]).await
}

/// The item form page, inside the site theme: the form built through the
/// pipeline, showing `posted` and `errors` when a submission failed (422).
async fn item_form_page(
    state: &AppState,
    session: &Session,
    user: &UserContext,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    item: Option<&crate::models::Item>,
    posted: Option<&crate::content::submission::PostedForm>,
    errors: &[crate::form::ValidationError],
) -> Result<Response, AppError> {
    let (action, heading, extra) = match item {
        Some(item) => {
            // The URL alias, inside the form so it is submitted.
            let source = format!("/item/{}", item.id);
            let current_alias = UrlAlias::get_canonical_alias(state.db(), &source)
                .await
                .unwrap_or(None)
                .unwrap_or_default();
            let mut extra = vec![(
                "url_alias".to_string(),
                crate::form::FormElement::textfield()
                    .title("URL Alias")
                    .placeholder("/about-us")
                    .description("Human-readable URL path. Leave empty for default (/item/id)")
                    .default_value(current_alias)
                    .weight(crate::content::LOG_WEIGHT - 10),
            )];
            extra.extend(super::item_form::stage_element(state, user, item).await);
            (
                format!("/item/{}/edit", item.id),
                format!("Edit: {}", item.title),
                extra,
            )
        }
        None => (
            format!("/item/add/{}", content_type.machine_name),
            format!("Create {}", content_type.label),
            Vec::new(),
        ),
    };

    let mut form =
        super::item_form::build(state, session, user, content_type, item, &action, extra)
            .await
            .map_err(|e| AppError::internal_ctx(e, "build item form"))?;
    if let Some(posted) = posted {
        super::item_form::redisplay(&mut form, posted);
    }
    let form_html = state
        .theme()
        .render_form_with_errors(&form, errors)
        .map_err(|e| AppError::internal_ctx(e, "render item form"))?;

    let mut context = tera::Context::new();
    context.insert("title", &heading);
    context.insert("form_html", &form_html);
    super::helpers::inject_site_context(state, session, &mut context, &action).await;
    let html = state
        .theme()
        .tera()
        .render("item-form.html", &context)
        .map_err(|e| AppError::internal_ctx(anyhow::Error::from(e), "render item form page"))?;

    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    Ok((status, Html(html)).into_response())
}

/// The field map a submission writes, or the response that refuses it.
///
/// A form body goes through the pipeline (`item_form::process`: CSRF, the
/// form state, the submission builder, `tap_form_validate`); a refusal
/// re-renders the form with its errors. A JSON body keeps its contract: the
/// `X-CSRF-Token` header, and `fields` sent as is.
enum Submitted {
    /// Save this, then (for a form) dispatch `tap_form_submit` for this state.
    Fields(Option<serde_json::Value>, Option<crate::form::FormState>),
    /// The form, re-rendered with what refused it.
    Refused(Response),
}

async fn submitted_fields(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    user: &UserContext,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    item: Option<&crate::models::Item>,
    request: &ItemSubmission,
) -> Result<Submitted, AppError> {
    let Some(ref posted) = request.posted else {
        crate::routes::helpers::require_csrf_header(session, headers)
            .await
            .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;
        return Ok(Submitted::Fields(request.fields.clone(), None));
    };
    match super::item_form::process(state, session, user, content_type, item, posted).await {
        Ok((form_state, fields)) => Ok(Submitted::Fields(
            Some(serde_json::Value::Object(fields)),
            Some(form_state),
        )),
        Err(crate::form::ProcessError::Csrf) => {
            Err(AppError::forbidden("Invalid or missing CSRF token"))
        }
        Err(crate::form::ProcessError::Invalid(errors)) => Ok(Submitted::Refused(
            item_form_page(
                state,
                session,
                user,
                content_type,
                item,
                Some(posted),
                &errors,
            )
            .await?,
        )),
    }
}

/// Create a new item.
async fn create_item(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Extension(lang): Extension<ResolvedLanguage>,
    Path(item_type): Path<String>,
    request: ItemSubmission,
) -> Result<Response, AppError> {
    let user = get_user_context(&session, &state).await;

    // Check permission
    let permission = format!("create {item_type} content");
    if !user.can(&permission) {
        return Err(AppError::forbidden("Access denied"));
    }

    // Check content type exists
    let Some(content_type) = state.content_types().get(&item_type) else {
        return Err(AppError::not_found("content type"));
    };

    let (fields, form_state) = match submitted_fields(
        &state,
        &session,
        &headers,
        &user,
        &content_type,
        None,
        &request,
    )
    .await?
    {
        Submitted::Fields(fields, form_state) => (fields, form_state),
        Submitted::Refused(page) => return Ok(page),
    };

    let language = lang.0;
    // No stage: the default stage. Another is a move from the default, which
    // the service decides.
    let stage_id = request.requested_stage()?;

    // A create needs a title; on the JSON path this was previously enforced by
    // serde's required field, so the error is kept a 400 either way.
    let title = request
        .title
        .as_deref()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AppError::bad_request("Title is required"))?;

    let input = CreateItem {
        item_type: item_type.clone(),
        title,
        author_id: user.id,
        status: request.status,
        promote: None,
        sticky: None,
        fields,
        stage_id,
        language: Some(language),
        log: request.log.clone(),
    };

    let item = state
        .items()
        .create(input, &user)
        .await
        .map_err(|e| write_error(e, "create item"))?;

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

    if let (Some(form_state), Some(posted)) = (form_state, request.posted.as_ref()) {
        state
            .forms()
            .submitted(
                &form_state,
                posted,
                Some(item.id),
                &super::item_form::tap_state(&state, &user),
            )
            .await;
    }

    Ok(Json(ItemResponse {
        id: item.id,
        title: item.title,
        item_type: item.item_type,
        status: item.status,
    })
    .into_response())
}

/// Display edit item form.
async fn edit_item_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    let user = get_user_context(&session, &state).await;

    let item = state
        .items()
        .load(id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "load item"))?
        .ok_or_else(|| AppError::not_found("Item"))?;

    if !state
        .items()
        .check_access(&item, "edit", &user)
        .await
        .unwrap_or(false)
    {
        return Err(AppError::forbidden("Access denied"));
    }

    let content_type = state
        .content_types()
        .get(&item.item_type)
        .ok_or_else(|| AppError::not_found("content type"))?;

    item_form_page(
        &state,
        &session,
        &user,
        &content_type,
        Some(&item),
        None,
        &[],
    )
    .await
}

/// Update an existing item.
async fn update_item(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    request: ItemSubmission,
) -> Result<Response, AppError> {
    let user = get_user_context(&session, &state).await;

    // A form body is built over the stored fields, so it needs the item and its
    // type; a JSON body is sent as is.
    let (fields, form_state) = if request.posted.is_some() {
        let stored = state
            .items()
            .load(id)
            .await
            .map_err(|e| AppError::internal_ctx(e, "load item"))?
            .ok_or_else(|| AppError::not_found_id("item", id))?;
        let content_type = state
            .content_types()
            .get(&stored.item_type)
            .ok_or_else(|| AppError::not_found("content type"))?;
        match submitted_fields(
            &state,
            &session,
            &headers,
            &user,
            &content_type,
            Some(&stored),
            &request,
        )
        .await?
        {
            Submitted::Fields(fields, form_state) => (fields, form_state),
            Submitted::Refused(page) => return Ok(page),
        }
    } else {
        crate::routes::helpers::require_csrf_header(&session, &headers)
            .await
            .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;
        (request.fields.clone(), None)
    };
    let stage_id = request.requested_stage()?;

    let input = UpdateItem {
        stage_id,
        title: request.title.clone(),
        status: request.status,
        promote: None,
        sticky: None,
        fields,
        log: request.log.clone(),
    };

    match state.items().update(id, input, &user).await {
        Ok(Some(item)) => {
            // Handle URL alias update if provided
            if let Some(ref alias_path) = request.url_alias {
                let source = format!("/item/{id}");
                let alias_path = alias_path.trim();

                if alias_path.is_empty() {
                    // Delete existing alias if path is cleared
                    if let Err(e) = UrlAlias::delete_by_source(state.db(), &source).await {
                        tracing::warn!(error = %e, "failed to delete url alias");
                    }
                } else {
                    // Ensure alias starts with /
                    let alias_path = if alias_path.starts_with('/') {
                        alias_path.to_string()
                    } else {
                        format!("/{alias_path}")
                    };

                    // Create or update alias
                    if let Err(e) = UrlAlias::upsert_for_source(
                        state.db(),
                        &source,
                        &alias_path,
                        crate::models::stage::LIVE_STAGE_ID,
                        "en",
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to update url alias");
                    }
                }
            } else {
                // No explicit alias — auto-update from pathauto pattern
                if let Err(e) = crate::services::pathauto::update_alias_item(
                    state.db(),
                    item.id,
                    &item.title,
                    &item.item_type,
                    item.created,
                )
                .await
                {
                    tracing::warn!(error = %e, item_id = %item.id, "pathauto alias update failed");
                }
            }

            if let (Some(form_state), Some(posted)) = (form_state, request.posted.as_ref()) {
                state
                    .forms()
                    .submitted(
                        &form_state,
                        posted,
                        Some(item.id),
                        &super::item_form::tap_state(&state, &user),
                    )
                    .await;
            }

            Ok(Json(ItemResponse {
                id: item.id,
                title: item.title,
                item_type: item.item_type,
                status: item.status,
            })
            .into_response())
        }
        Ok(None) => Err(AppError::not_found_id("item", id)),
        Err(e) => Err(write_error(e, "update item")),
    }
}

/// Delete an item.
async fn delete_item(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = get_user_context(&session, &state).await;

    // Verify CSRF token from header
    crate::routes::helpers::require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;

    match state.items().delete(id, &user).await {
        Ok(true) => Ok(Json(serde_json::json!({"deleted": true}))),
        Ok(false) => Err(AppError::not_found_id("item", id)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("access denied") {
                Err(AppError::forbidden("Access denied"))
            } else {
                Err(AppError::internal_ctx(e, "delete item"))
            }
        }
    }
}

/// Load an item and the revisions a revision page asks for, as the session's
/// user, or the response that refuses them.
///
/// Every revision read route starts here: a login, then
/// [`crate::content::ItemService::load_revisions`], which holds the item to
/// `view` (404) then `edit` (403), checks every revision id against the item in
/// the path, and drops the fields the user may not view.
async fn revisions_for_session(
    state: &AppState,
    session: &Session,
    id: Uuid,
    which: RevisionSelection<'_>,
) -> Result<ItemRevisions, Response> {
    super::helpers::require_login(state, session).await?;
    let user = get_user_context(session, state).await;
    state
        .items()
        .load_revisions(id, which, &user)
        .await
        .map_err(|refusal| match refusal {
            RevisionAccess::NotFound => super::helpers::render_not_found(),
            RevisionAccess::Forbidden => super::helpers::render_forbidden(
                "You do not have permission to read this item's revision history.",
            ),
            RevisionAccess::Failed(e) => {
                tracing::error!(error = %e, item_id = %id, "failed to load revisions");
                super::helpers::render_server_error("Failed to load revisions")
            }
        })
}

/// The name of every author of `revisions`, in one query however many there
/// are. A deleted account's revisions carry the anonymous author, whose row
/// names it; an id with no row at all is shown as that author too.
async fn revision_authors(
    state: &AppState,
    revisions: &[crate::models::ItemRevision],
) -> std::collections::HashMap<Uuid, String> {
    let mut ids: Vec<Uuid> = revisions.iter().map(|r| r.author_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut names = state.users().names_by_ids(&ids).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load revision authors");
        std::collections::HashMap::new()
    });
    for id in ids {
        names.entry(id).or_insert_with(|| "anonymous".to_string());
    }
    names
}

/// A revision as the revision templates show it.
fn revision_summary(
    revision: &crate::models::ItemRevision,
    authors: &std::collections::HashMap<Uuid, String>,
    current: Option<Uuid>,
) -> serde_json::Value {
    let date = chrono::DateTime::from_timestamp(revision.created, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    serde_json::json!({
        "id": revision.id,
        "date": date,
        "title": revision.title,
        "log": revision.log,
        "author": authors.get(&revision.author_id),
        "is_current": Some(revision.id) == current,
    })
}

/// List revision history for an item.
///
/// Requires authentication — revision history may contain draft titles and
/// internal log messages not intended for anonymous visitors — and answers to
/// `view` then `edit` on the item, as every revision route does.
async fn list_revisions(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> Response {
    let ItemRevisions { item, revisions } =
        match revisions_for_session(&state, &session, id, RevisionSelection::All).await {
            Ok(loaded) => loaded,
            Err(refusal) => return refusal,
        };

    let authors = revision_authors(&state, &revisions).await;
    let rev_data: Vec<serde_json::Value> = revisions
        .iter()
        .map(|rev| revision_summary(rev, &authors, item.current_revision_id))
        .collect();

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("path", &format!("/item/{id}/revisions"));
    context.insert("item_id", &id);
    context.insert("item_title", &item.title);
    context.insert("revisions", &rev_data);
    context.insert("csrf_token", &csrf_token);

    super::helpers::render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/revisions.html",
        context,
    )
    .await
}

/// Preview one past revision: its title and fields, rendered as the item page
/// renders them, without dispatching a view tap or writing anything.
async fn preview_revision(
    State(state): State<AppState>,
    session: Session,
    Path((id, rev_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let ItemRevisions { item, revisions } = match revisions_for_session(
        &state,
        &session,
        id,
        RevisionSelection::Ids(&[rev_id]),
    )
    .await
    {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    let Some(revision) = revisions.first() else {
        return super::helpers::render_not_found();
    };

    let content_type_fields = state
        .content_types()
        .get(&item.item_type)
        .map(|ct| ct.fields.clone())
        .unwrap_or_default();
    let fields_html = render_item_fields(&state, &revision.fields, &content_type_fields);
    let authors = revision_authors(&state, &revisions).await;

    let mut context = tera::Context::new();
    context.insert("path", &format!("/item/{id}/revisions/{rev_id}"));
    context.insert("item_id", &id);
    context.insert("item_title", &item.title);
    context.insert(
        "revision",
        &revision_summary(revision, &authors, item.current_revision_id),
    );
    context.insert("current_revision_id", &item.current_revision_id);
    context.insert("fields_html", &fields_html);

    super::helpers::render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/revision-preview.html",
        context,
    )
    .await
}

/// The two revisions a compare is asked for.
#[derive(Debug, Deserialize)]
struct CompareQuery {
    from: Uuid,
    to: Uuid,
}

/// Compare two revisions of an item, field by field.
///
/// Computed from the two filtered `fields` snapshots by
/// [`crate::content::revision_compare::compare_revisions`]; the older of the two
/// is always the left side, whichever order they were picked in.
async fn compare_revisions(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(query): Query<CompareQuery>,
) -> Response {
    let ItemRevisions { item, revisions } = match revisions_for_session(
        &state,
        &session,
        id,
        RevisionSelection::Ids(&[query.from, query.to]),
    )
    .await
    {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    let [first, second] = revisions.as_slice() else {
        return super::helpers::render_not_found();
    };
    let (from, to) = if (second.created, second.id) < (first.created, first.id) {
        (second, first)
    } else {
        (first, second)
    };

    let content_type_fields = state
        .content_types()
        .get(&item.item_type)
        .map(|ct| ct.fields.clone())
        .unwrap_or_default();
    let comparison = crate::content::revision_compare::compare_revisions(
        &from.title,
        &from.fields,
        &to.title,
        &to.fields,
        &content_type_fields,
    );
    let authors = revision_authors(&state, &revisions).await;

    let mut context = tera::Context::new();
    context.insert("path", &format!("/item/{id}/revisions/compare"));
    context.insert("item_id", &id);
    context.insert("item_title", &item.title);
    context.insert(
        "from",
        &revision_summary(from, &authors, item.current_revision_id),
    );
    context.insert(
        "to",
        &revision_summary(to, &authors, item.current_revision_id),
    );
    context.insert("comparison", &comparison);

    super::helpers::render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/revision-compare.html",
        context,
    )
    .await
}

/// Revert to a previous revision.
async fn revert_revision(
    State(state): State<AppState>,
    session: Session,
    Path((id, rev_id)): Path<(Uuid, Uuid)>,
    Form(form): Form<CsrfOnlyForm>,
) -> Result<impl IntoResponse, AppError> {
    let user = get_user_context(&session, &state).await;

    // Verify CSRF token from form body
    crate::routes::helpers::require_csrf(&session, &form.token)
        .await
        .map_err(|_| AppError::forbidden("Invalid or expired form token. Please try again."))?;

    match state.items().revert_to_revision(id, rev_id, &user).await {
        Ok(_) => Ok(Redirect::to(&format!("/item/{id}/revisions"))),
        Err(e) => Err(write_error(e, "revert revision")),
    }
}

/// List all content types (API endpoint).
async fn list_content_types(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.content_types().type_names())
}

/// List items by type (API endpoint).
async fn list_items_by_type(
    State(state): State<AppState>,
    session: Session,
    Path(item_type): Path<String>,
) -> Result<Json<Vec<ItemResponse>>, AppError> {
    let items = state
        .items()
        .list_by_type(&item_type)
        .await
        .map_err(|e| AppError::internal_ctx(e, "list items by type"))?;

    // Enforce item-level access via the seam (FR-8 Story 3.3). `ItemResponse`
    // omits fields, so this drops inaccessible items (field-filtering is moot).
    let user = get_user_context(&session, &state).await;
    let count = items.len();
    let items = state
        .items()
        .filter_page_for_view(items, &user, "view", count)
        .await;

    Ok(Json(
        items
            .into_iter()
            .map(|i| ItemResponse {
                id: i.id,
                title: i.title,
                item_type: i.item_type,
                status: i.status,
            })
            .collect(),
    ))
}

// =============================================================================
// JSON API Endpoints
// =============================================================================

/// Get a single item by ID (JSON API).
///
/// GET /api/item/{id}?include=author
async fn get_item_api(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(query): Query<GetItemQuery>,
) -> Result<Json<ItemApiResponse>, AppError> {
    // Load item through the shared access-aware seam (FR-8 Story 3.3): enforces
    // item-level access (absent ⇒ 404) and drops fields this viewer may not see.
    let user = get_user_context(&session, &state).await;
    let item = state
        .items()
        .load_for_view_filtered(id, &user, "view")
        .await
        .map_err(|e| AppError::internal_ctx(e, "load item"))?
        .ok_or_else(|| AppError::not_found_id("item", id))?;

    // Check if we should include author
    let include_author = query
        .include
        .as_ref()
        .map(|s| s.split(',').any(|part| part.trim() == "author"))
        .unwrap_or(false);

    let author = if include_author {
        match state.users().find_by_id(item.author_id).await {
            Ok(Some(user)) => Some(AuthorResponse {
                id: user.id,
                name: user.name,
            }),
            _ => None,
        }
    } else {
        None
    };

    Ok(Json(ItemApiResponse {
        id: item.id,
        item_type: item.item_type,
        title: item.title,
        status: item.status,
        author_id: item.author_id,
        author,
        created: item.created,
        changed: item.changed,
        promote: item.promote,
        sticky: item.sticky,
        fields: item.fields,
        stage_id: item.stage_id,
        item_group_id: item.item_group_id,
    }))
}

/// List items with filtering and pagination (JSON API).
///
/// GET /api/items?type=article&status=1&page=1&per_page=20&include=author
async fn list_items_api(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<ListItemsQuery>,
) -> Result<Json<PaginatedResponse<ItemApiResponse>>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * per_page;

    // Check if we should include author
    let include_author = query
        .include
        .as_ref()
        .map(|s| s.split(',').any(|part| part.trim() == "author"))
        .unwrap_or(false);

    // Build query with filters
    let (items, total) = state
        .items()
        .list_filtered(
            query.item_type.as_deref(),
            query.status,
            query.author_id,
            per_page,
            offset,
        )
        .await
        .map_err(|e| AppError::internal_ctx(e, "list items"))?;

    // Route the page through the shared access-aware seam (FR-8 Story 3.3):
    // drop items this viewer cannot see and fields they may not see. `total`
    // remains the pre-filter count (accurate access-aware totals are the gather
    // over-fetch layer's concern, Story 3.4).
    let user = get_user_context(&session, &state).await;
    let page_len = items.len();
    let items = state
        .items()
        .filter_page_for_view(items, &user, "view", page_len)
        .await;

    // Optionally load authors
    let mut author_cache: std::collections::HashMap<Uuid, AuthorResponse> =
        std::collections::HashMap::new();
    if include_author {
        let author_ids: Vec<Uuid> = items.iter().map(|i| i.author_id).collect();
        for author_id in author_ids {
            if !author_cache.contains_key(&author_id)
                && let Ok(Some(user)) = state.users().find_by_id(author_id).await
            {
                author_cache.insert(
                    author_id,
                    AuthorResponse {
                        id: user.id,
                        name: user.name,
                    },
                );
            }
        }
    }

    let total_pages = (total as f64 / per_page as f64).ceil() as i64;

    let items_response: Vec<ItemApiResponse> = items
        .into_iter()
        .map(|item| {
            let author = if include_author {
                author_cache.get(&item.author_id).cloned()
            } else {
                None
            };
            ItemApiResponse {
                id: item.id,
                item_type: item.item_type,
                title: item.title,
                status: item.status,
                author_id: item.author_id,
                author,
                created: item.created,
                changed: item.changed,
                promote: item.promote,
                sticky: item.sticky,
                fields: item.fields,
                stage_id: item.stage_id,
                item_group_id: item.item_group_id,
            }
        })
        .collect();

    Ok(Json(PaginatedResponse {
        items: items_response,
        pagination: PaginationMeta {
            total,
            page,
            per_page,
            total_pages,
        },
    }))
}
