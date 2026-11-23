//! The versioned JSON API (v1): every kernel JSON route, under `/api/v1`.
//!
//! [`kernel_api`] is the list; the routes themselves are declared beside
//! their handlers, in their modules, through [`super::api::ApiRoutes`]. Every
//! list answers with [`Page`] (`{ data, total, page, per_page }`) and every
//! error with an [`AppError`](crate::error::AppError) body.

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Serialize;
use std::collections::HashMap;
use tower_sessions::Session;
use uuid::Uuid;

use super::api::{ApiRoutes, doc};
use crate::file::service::{FileStatus, MediaScope};
use crate::pagination::{Page, PageParams};
use crate::state::AppState;

/// Every kernel JSON route, declared with its OpenAPI description.
///
/// The one list both the router ([`router`]) and the document
/// ([`crate::routes::route_metadata::RouteRegistry`]) are built from, so the
/// document lists exactly what is served.
pub fn kernel_api() -> ApiRoutes {
    ApiRoutes::new("api")
        .get(
            "/openapi.json",
            openapi_spec,
            doc("This document: the kernel's API and every enabled plugin's").alias(),
        )
        .get(
            "/user/export",
            export_user_data,
            doc("Export user data (GDPR Article 20)")
                .query("user_id", "User UUID (admin only, defaults to self)"),
        )
        .get(
            "/items/autocomplete",
            autocomplete_items,
            doc("Search items by type and title for autocomplete; the first page only")
                .required("type", "Content type machine name, or `category_term`")
                .required("q", "Title prefix search query")
                .paged(10, 50)
                .query(
                    "item_type",
                    "With type=category_term: the content type whose field is being filled in",
                )
                .query(
                    "field",
                    "With type=category_term: the term reference field being filled in",
                )
                .query(
                    "item",
                    "With type=category_term: the item being edited, if any",
                )
                .query(
                    "owner",
                    "With type=category_term: the form's value of the field's owner field, \
                     for a field limited to the item's owner",
                ),
        )
        .get(
            "/media/browse",
            browse_media,
            doc("Browse the media library")
                .query("type", "`image`, `document` or `all` (default)")
                .query("q", "Case-insensitive filename search")
                .query("sort", "`newest` (default), `oldest`, `name` or `size`")
                .paged(24, 100),
        )
        .get(
            "/page-builder/components",
            list_page_builder_components,
            doc("The page builder component registry"),
        )
        .post(
            "/page-builder/preview",
            preview_page_builder_component,
            doc("Render one page builder component as a page would (`use page builder`)"),
        )
        .merge(super::item::api())
        .merge(super::search::api())
        .merge(super::api_search::api())
        .merge(super::gather::api())
        .merge(super::batch::api())
        .merge(super::api_token::api())
        .merge(super::api_ai_assist::api())
        .merge(super::api_chat::api())
        .merge(super::assistant::api())
        .merge(super::category::api())
        .merge(super::comment::api())
        .merge(super::lock::api())
        .merge(super::file::block_editor_api())
}

/// Create the v1 API router: [`kernel_api`] under `/api/v1`, with the
/// deprecated unversioned aliases.
pub fn router(state: &AppState) -> Router<AppState> {
    kernel_api().into_router(state)
}

// -------------------------------------------------------------------------
/// Serve the OpenAPI 3.0 specification: the kernel's API routes and every
/// enabled plugin's `api` routes, built once at startup
/// ([`AppState::openapi`]).
async fn openapi_spec(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        state.openapi().to_string(),
    )
}

// -------------------------------------------------------------------------
// User data export (GDPR Article 20)
// -------------------------------------------------------------------------

/// Export the authenticated user's data as JSON.
///
/// Returns user profile, authored items (PII fields only), comments,
/// and file uploads. Admin users can export any user's data by
/// providing a `user_id` query parameter.
async fn export_user_data(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    // Require authentication
    let requester = match crate::routes::helpers::require_login_json(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal.into_response(),
    };
    let requester_id = requester.id;

    // Determine which user to export
    let target_user_id = if let Some(uid_str) = params.get("user_id") {
        let Ok(uid) = Uuid::parse_str(uid_str) else {
            return crate::error::AppError::with_status(StatusCode::BAD_REQUEST, "Invalid user_id")
                .into_response();
        };

        // Only admins can export other users' data
        if uid != requester_id && !requester.is_admin {
            return crate::error::AppError::forbidden("Admin access required").into_response();
        }
        uid
    } else {
        requester_id
    };

    // Load user profile
    let user = match crate::models::User::find_by_id(state.db(), target_user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return crate::error::AppError::with_status(StatusCode::NOT_FOUND, "User not found")
                .into_response();
        }
        Err(_) => {
            return crate::error::AppError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database error",
            )
            .into_response();
        }
    };

    // Collect user-authored items with only personal_data fields
    let items = match crate::models::Item::list_by_author(state.db(), target_user_id).await {
        Ok(items) => {
            let mut filtered = Vec::new();
            for item in items {
                // Load content type to filter personal_data fields
                let personal_fields = get_personal_fields(state.db(), &item.item_type).await;
                let mut item_data = serde_json::json!({
                    "id": item.id,
                    "type": item.item_type,
                    "title": item.title,
                    "created": item.created,
                    "changed": item.changed,
                });
                if !personal_fields.is_empty() {
                    let mut pii_fields = serde_json::Map::new();
                    if let Some(obj) = item.fields.as_object() {
                        for name in &personal_fields {
                            if let Some(val) = obj.get(name) {
                                pii_fields.insert(name.clone(), val.clone());
                            }
                        }
                    }
                    item_data["personal_fields"] = serde_json::Value::Object(pii_fields);
                }
                filtered.push(item_data);
            }
            serde_json::Value::Array(filtered)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load user items for export");
            serde_json::json!([])
        }
    };

    // Collect user-authored comments
    let comments = match sqlx::query_as::<_, crate::models::Comment>(
        "SELECT * FROM comment WHERE author_id = $1 ORDER BY created DESC",
    )
    .bind(target_user_id)
    .fetch_all(state.db())
    .await
    {
        Ok(rows) => serde_json::to_value(&rows).unwrap_or(serde_json::json!([])),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load user comments for export");
            serde_json::json!([])
        }
    };

    // Collect user-uploaded files (filename, size, type — no content)
    let files: serde_json::Value = match sqlx::query(
        "SELECT id, filename, filemime, filesize, created FROM file_managed WHERE owner_id = $1 ORDER BY created DESC",
    )
    .bind(target_user_id)
    .fetch_all(state.db())
    .await
    {
        Ok(rows) => {
            let entries: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    use sqlx::Row;
                    serde_json::json!({
                        "id": r.get::<Uuid, _>("id"),
                        "filename": r.get::<String, _>("filename"),
                        "filemime": r.get::<String, _>("filemime"),
                        "filesize": r.get::<i64, _>("filesize"),
                        "created": r.get::<i64, _>("created"),
                    })
                })
                .collect();
            serde_json::Value::Array(entries)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load user files for export");
            serde_json::json!([])
        }
    };

    // Build export payload
    let export = serde_json::json!({
        "user": {
            "id": user.id,
            "name": user.name,
            "mail": user.mail,
            "created": user.created,
            "language": user.language,
            "consent_given": user.consent_given,
            "consent_date": user.consent_date,
            "consent_version": user.consent_version,
        },
        "items": items,
        "comments": comments,
        "files": files,
    });

    (StatusCode::OK, Json(export)).into_response()
}

/// Autocomplete endpoint for RecordReference fields.
///
/// `GET /api/v1/items/autocomplete?type=article&q=rust&per_page=10`
///
/// Returns a page of `{"id": "uuid", "title": "..."}` for items matching the
/// query.
/// For `type=category_term` the answer is category terms instead, in the same
/// shape, decided by `autocomplete_terms`.
///
/// Answers as the viewer. The query is a coarse candidate filter — type, title
/// prefix, published — and
/// [`filter_page_for_view`](crate::content::ItemService::filter_page_for_view)
/// decides what
/// of that the caller may actually see. Without it the endpoint took no session
/// at all and `status = 1` was the whole rule, so any caller could enumerate
/// the titles of published items on internal stages a prefix at a time.
async fn autocomplete_items(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
    Query(paging): Query<PageParams>,
) -> impl IntoResponse {
    let item_type = params.get("type").map(String::as_str).unwrap_or("");
    let query = params.get("q").map(String::as_str).unwrap_or("");
    // Suggestions are the first page only: a widget narrows by typing more.
    let window = PageParams {
        page: None,
        offset: None,
        ..paging
    }
    .window(10, 50);
    let limit = window.per_page;
    let answer = |data: Vec<serde_json::Value>| {
        let total = data.len() as u64;
        Json(Page::new(data, total, window)).into_response()
    };

    if item_type.is_empty() || query.is_empty() {
        return answer(Vec::new());
    }

    // Category terms are not items: a term reference field is answered from
    // the terms, as the field allows. See `autocomplete_terms`.
    if item_type == TERM_TARGET {
        let user = crate::routes::item::get_user_context(&session, &state).await;
        let results = autocomplete_terms(&state, &user, &params, query, limit).await;
        return answer(results);
    }

    // Search by title using ILIKE for case-insensitive prefix matching
    let pattern = format!("{}%", query.replace('%', "\\%").replace('_', "\\_"));

    // Over-fetch, bounded, so a page the seam thins out still has something in
    // it. Three times the asked-for page, capped: the cost of an access
    // decision is paid per candidate, and this is a keystroke-rate endpoint.
    let candidate_limit = (limit * 3).min(150);
    let user = crate::routes::item::get_user_context(&session, &state).await;

    match sqlx::query_as::<_, crate::models::Item>(
        "SELECT id, current_revision_id, type, title, author_id, status, created, changed, \
         promote, sticky, fields, stage_id, language, item_group_id, retention_days \
         FROM item WHERE type = $1 AND title ILIKE $2 AND status = 1 ORDER BY title LIMIT $3",
    )
    .bind(item_type)
    .bind(&pattern)
    .bind(candidate_limit)
    .fetch_all(state.db())
    .await
    {
        Ok(candidates) => {
            let page_size = usize::try_from(limit).unwrap_or(0);
            let visible = state
                .items()
                .filter_page_for_view(candidates, &user, "view", page_size)
                .await;
            let results: Vec<serde_json::Value> = visible
                .iter()
                .map(|item| {
                    serde_json::json!({
                        "id": item.id,
                        "title": item.title
                    })
                })
                .collect();
            answer(results)
        }
        Err(e) => {
            tracing::warn!(error = %e, "autocomplete query failed");
            answer(Vec::new())
        }
    }
}

/// The `RecordReference` target type of a category term field.
const TERM_TARGET: &str = "category_term";

/// The suggestions for a `category_term` reference field: terms whose name
/// starts with `q`, limited to what the field's save check accepts, for a
/// caller who may edit the field.
///
/// Terms are not items, so the item query above has nothing to say about
/// them. The widget sends the field it is filling in (`item_type`, `field`),
/// the item when it is editing one (`item`) and the current value of the
/// field's owner field (`owner`). The caller must be able to edit that field
/// as the item form decides it: the type's create permission, or edit access
/// to the item, and the field's own view and edit decisions. Without that
/// context, or without that access, the answer is empty.
///
/// Which vocabularies are searched is the field's: one limited to the item's
/// owner (`settings.vocabulary_owner`) searches the vocabularies
/// [`OwnerLimit::admits`](crate::content::owned_vocabulary::OwnerLimit::admits),
/// the rule the save path enforces; an open field searches the site-wide
/// vocabularies, never an owner's.
async fn autocomplete_terms(
    state: &AppState,
    user: &crate::tap::UserContext,
    params: &HashMap<String, String>,
    query: &str,
    limit: i64,
) -> Vec<serde_json::Value> {
    use crate::content::owned_vocabulary;
    use trovato_sdk::types::FieldType;

    let param = |key: &str| params.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());
    let (Some(item_type), Some(field_name)) = (param("item_type"), param("field")) else {
        return Vec::new();
    };
    let Some(content_type) = state.content_types().get(item_type) else {
        return Vec::new();
    };
    let Some(field) = content_type.fields.iter().find(|f| {
        f.field_name == field_name
            && matches!(&f.field_type, FieldType::RecordReference(t) if t == TERM_TARGET)
    }) else {
        return Vec::new();
    };

    // The item, when the form is editing one, decided as the edit form is.
    let existing = match param("item") {
        Some(id) => {
            let Ok(id) = id.parse::<Uuid>() else {
                return Vec::new();
            };
            let Ok(Some(item)) = state.items().load(id).await else {
                return Vec::new();
            };
            if item.item_type != item_type
                || !state
                    .items()
                    .check_access(&item, "edit", user)
                    .await
                    .unwrap_or(false)
            {
                return Vec::new();
            }
            Some(item)
        }
        None => {
            if !user.can(&format!("create {item_type} content")) {
                return Vec::new();
            }
            None
        }
    };

    let limit_rule = owned_vocabulary::limit_of(&field.settings);
    let mut names = vec![field.field_name.clone()];
    if let Some(rule) = &limit_rule {
        names.push(rule.owner_field.clone());
    }
    let may_view = state
        .items()
        .field_access_decisions(user, item_type, &names, "view")
        .await;
    let may_edit = state
        .items()
        .field_access_decisions(user, item_type, &names, "edit")
        .await;
    let editable = |name: &str| {
        may_view.get(name).copied().unwrap_or(true) && may_edit.get(name).copied().unwrap_or(true)
    };
    if !editable(&field.field_name) {
        return Vec::new();
    }

    let Ok(vocabularies) = state.categories().list_categories().await else {
        return Vec::new();
    };
    let vocabularies: Vec<String> = match &limit_rule {
        Some(rule) => {
            // The owner the save would see: the form's, when this editor may
            // write the owner field, else the stored one (the write gate
            // copies a field they may not edit back from the item).
            let posted = param("owner")
                .filter(|_| editable(&rule.owner_field))
                .map(|owner| {
                    let mut posted = serde_json::Map::new();
                    posted.insert(rule.owner_field.clone(), owner.into());
                    serde_json::Value::Object(posted)
                });
            let Some(owner) =
                rule.item_owner(posted.as_ref(), existing.as_ref().map(|item| &item.fields))
            else {
                return Vec::new();
            };
            vocabularies
                .into_iter()
                .filter(|v| rule.admits(&owner, v.owner()))
                .map(|v| v.id)
                .collect()
        }
        None => vocabularies
            .into_iter()
            .filter(|v| v.owner().is_none())
            .map(|v| v.id)
            .collect(),
    };

    let page_size = usize::try_from(limit).unwrap_or(0);
    match state
        .categories()
        .search_tags(&vocabularies, query, page_size)
        .await
    {
        Ok(terms) => terms
            .into_iter()
            .map(|t| serde_json::json!({ "id": t.id, "title": t.label }))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "term autocomplete failed");
            Vec::new()
        }
    }
}

// -------------------------------------------------------------------------
// Media browse endpoint
// -------------------------------------------------------------------------

/// A single media file in browse results.
#[derive(Debug, Serialize)]
struct MediaItem {
    /// File UUID.
    id: String,
    /// Original filename.
    filename: String,
    /// MIME type (e.g. `image/jpeg`).
    mime_type: String,
    /// File size in bytes.
    size: i64,
    /// Public URL to serve/download the file.
    url: String,
    /// Thumbnail URL for images, `None` for non-image files.
    thumbnail_url: Option<String>,
    /// Upload timestamp (Unix epoch).
    created: i64,
}

/// Browse the media library with filtering, search, and pagination.
///
/// `GET /api/v1/media/browse?page=1&per_page=24&type=image&q=logo&sort=newest`
///
/// Query parameters:
/// - `page`, `per_page` — the page ([`PageParams`]; default 24, max 100)
/// - `type` — MIME type prefix filter: `image`, `document`, or `all` (default)
/// - `q` — case-insensitive filename search
/// - `sort` — `newest` (default), `oldest`, `name`, `size`
async fn browse_media(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
    Query(paging): Query<PageParams>,
) -> impl IntoResponse {
    // Require authentication
    let user_id = match crate::routes::helpers::require_login_json(&state, &session).await {
        Ok(user) => user.id,
        Err(refusal) => return refusal.into_response(),
    };

    // File administrators browse every file, as on the admin file screens;
    // everyone else sees their own uploads and files on published live items.
    let viewer = crate::routes::item::get_user_context(&session, &state).await;
    let scope = if viewer.can("access files") || viewer.can("administer files") {
        MediaScope::All
    } else {
        MediaScope::VisibleTo(user_id)
    };

    let window = paging.window(24, 100);
    let sort = params.get("sort").map(String::as_str).unwrap_or("newest");
    let search = params.get("q").map(String::as_str);

    // Map the `type` parameter to a MIME prefix
    let mime_prefix: Option<&str> = match params.get("type").map(String::as_str) {
        Some("image") => Some("image/"),
        Some("document") => Some("application/"),
        _ => None,
    };

    let total = match state
        .files()
        .count_filtered_media(Some(FileStatus::Permanent), mime_prefix, search, scope)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "failed to count media");
            return crate::error::AppError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to browse media",
            )
            .into_response();
        }
    };

    let files = match state
        .files()
        .list_filtered_media(
            Some(FileStatus::Permanent),
            mime_prefix,
            search,
            scope,
            sort,
            window.per_page,
            window.offset,
        )
        .await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "failed to list media");
            return crate::error::AppError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to browse media",
            )
            .into_response();
        }
    };

    let storage = state.files().storage();
    let items: Vec<MediaItem> = files
        .iter()
        .map(|f| {
            let url = storage.public_url(&f.uri);
            let thumbnail_url = if f.filemime.starts_with("image/") {
                Some(format!(
                    "/files/styles/thumbnail/{}",
                    f.uri.strip_prefix("local://").unwrap_or(&f.uri)
                ))
            } else {
                None
            };
            MediaItem {
                id: f.id.to_string(),
                filename: f.filename.clone(),
                mime_type: f.filemime.clone(),
                size: f.filesize,
                url,
                thumbnail_url,
                created: f.created,
            }
        })
        .collect();

    Json(Page::new(items, u64::try_from(total).unwrap_or(0), window)).into_response()
}

/// Get field names marked `personal_data: true` for a content type.
///
/// Returns an empty vec if the type is not found or has no PII fields.
async fn get_personal_fields(pool: &sqlx::PgPool, item_type: &str) -> Vec<String> {
    let Ok(Some(db_type)) = crate::models::ItemType::find_by_type(pool, item_type).await else {
        return Vec::new();
    };

    let fields: Vec<trovato_sdk::types::FieldDefinition> = db_type
        .settings
        .get("fields")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    fields
        .into_iter()
        .filter(|f| f.personal_data)
        .map(|f| f.field_name)
        .collect()
}

// -------------------------------------------------------------------------
// Page builder component registry
// -------------------------------------------------------------------------

/// GET /api/v1/page-builder/components — list available page builder components.
///
/// Returns the component registry for the Puck editor to initialize with: the
/// standard components and those of the plugins enabled at startup, in the
/// editor's order, each with its fields (type, label, limits, options,
/// defaults), zones, variant classes and stylesheets.
async fn list_page_builder_components(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "data": state.page_builder_components().all(),
    }))
}

/// Body of `POST /api/v1/page-builder/preview`.
#[derive(Debug, serde::Deserialize)]
struct PreviewRequest {
    /// One component as a page stores it: `{ type, props, zones? }`.
    component: serde_json::Value,
}

/// POST /api/v1/page-builder/preview — render one component as a page would.
///
/// The editor shows a component it has no React preview for (any plugin's)
/// with this HTML, so the canvas is what the page will be: the same template,
/// the same normalization, the same sanitizer, the references resolved as the
/// signed-in editor sees them. The answer also carries the component's
/// validation errors, so the editor can show them before a save refuses them.
///
/// Requires `use page builder`. It takes no CSRF token: it changes nothing,
/// and CSRF tokens are single use, so checking one would spend the token the
/// item form it sits in is about to post (the editor reads the page's token).
/// A cross-site page cannot use it either: the body must be
/// `application/json`, which a plain form cannot send, and the answer is not
/// readable across origins.
async fn preview_page_builder_component(
    State(state): State<AppState>,
    session: Session,
    Json(request): Json<PreviewRequest>,
) -> axum::response::Response {
    if let Err(refusal) =
        crate::routes::helpers::require_permission_json(&state, &session, "use page builder").await
    {
        return refusal.into_response();
    }
    let viewer = crate::routes::item::get_user_context(&session, &state).await;
    let page = serde_json::json!({ "root": {}, "content": [request.component] });
    let errors = state.page_builder_components().validate_page(&page);
    match crate::routes::helpers::render_page_builder_field(&state, &viewer, &page).await {
        Ok(rendered) => Json(serde_json::json!({
            "html": rendered.html,
            "stylesheets": rendered.stylesheets,
            "errors": errors,
        }))
        .into_response(),
        Err(e) => crate::error::AppError::bad_request(format!("{e:#}")).into_response(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeSet;

    /// Every JSON route the kernel serves is listed in the OpenAPI document.
    ///
    /// The document is built from [`super::kernel_api`], the same declarations
    /// the router is built from, so a route registered through them cannot be
    /// missing from it. What could still be missing is a route registered some
    /// other way. So: no file under `src/routes` registers an `/api` path with
    /// `.route(`, and every `api()` a route module declares is merged into
    /// [`super::kernel_api`].
    #[test]
    fn every_served_api_route_is_listed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes");
        let kernel_api = std::fs::read_to_string(dir.join("api_v1.rs")).unwrap();
        let mut bypassing = Vec::new();
        let mut unmerged = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
            let text = std::fs::read_to_string(&path).unwrap();
            let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.contains(".route(\"/api") {
                bypassing.push(name.clone());
            }
            if name == "api_v1" {
                continue; // this test's own text names the declarations
            }
            for (decl, call) in [
                ("pub fn api() -> super::api::ApiRoutes", "api()"),
                (
                    "pub fn block_editor_api() -> super::api::ApiRoutes",
                    "block_editor_api()",
                ),
            ] {
                if text.contains(decl) && !kernel_api.contains(&format!("super::{name}::{call}")) {
                    unmerged.push(format!("{name}::{call}"));
                }
            }
        }
        assert!(
            bypassing.is_empty(),
            "declare /api routes through ApiRoutes, not .route(): {bypassing:?}"
        );
        assert!(
            unmerged.is_empty(),
            "merge these into kernel_api(): {unmerged:?}"
        );
    }

    /// Each method and path is declared once, every route is under
    /// `/api/v1`, and every alias is the `/api` spelling of a versioned path.
    #[test]
    fn the_document_lists_each_route_once_under_the_prefix() {
        let docs = super::kernel_api().docs();
        let mut seen = BTreeSet::new();
        for doc in &docs {
            assert!(
                seen.insert((doc.method.clone(), doc.path.clone())),
                "declared twice: {} {}",
                doc.method,
                doc.path
            );
            if doc.deprecated {
                let versioned = doc.path.replacen("/api/", "/api/v1/", 1);
                assert!(
                    docs.iter()
                        .any(|d| !d.deprecated && d.path == versioned && d.method == doc.method),
                    "{} is an alias of nothing",
                    doc.path
                );
            } else {
                assert!(doc.path.starts_with("/api/v1/"), "{}", doc.path);
            }
        }
        // The old unversioned search is folded into the one search.
        assert!(seen.contains(&("GET".to_string(), "/api/v1/search".to_string())));
        assert!(seen.contains(&("GET".to_string(), "/api/search".to_string())));
    }
}
