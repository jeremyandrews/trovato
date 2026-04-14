//! Versioned REST API (v1) for content management.
//!
//! Provides a stable, paginated JSON API with envelope responses.
//! All list endpoints return `{ data, total, page, per_page }`.
//! Single-resource endpoints return `{ data }`.
//! Error endpoints return `{ error, status }`.

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use serde::Serialize;
use std::collections::HashMap;
use tower_sessions::Session;
use uuid::Uuid;

use crate::file::service::FileStatus;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

// -------------------------------------------------------------------------
// Response envelope types
// -------------------------------------------------------------------------

/// Paginated list envelope.
#[derive(Debug, Serialize)]
struct ListEnvelope<T: Serialize> {
    data: Vec<T>,
    total: u64,
    page: i64,
    per_page: i64,
}

/// Error envelope.
#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: String,
    status: u16,
}

// -------------------------------------------------------------------------
// Query parameter types
// -------------------------------------------------------------------------

/// Common query params for list endpoints.
#[derive(Debug, serde::Deserialize)]
struct ListParams {
    #[serde(default = "default_page")]
    page: i64,
    #[serde(default = "default_per_page")]
    per_page: i64,
    q: Option<String>,
}

fn default_page() -> i64 {
    1
}

fn default_per_page() -> i64 {
    25
}

/// Create the v1 API router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/search", get(search))
        .route("/api/v1/user/export", get(export_user_data))
        .route("/api/v1/items/autocomplete", get(autocomplete_items))
        .route("/api/v1/media/browse", get(browse_media))
        .route("/api/openapi.json", get(openapi_spec))
        .route(
            "/api/v1/page-builder/components",
            get(list_page_builder_components),
        )
        .layer(axum::middleware::from_fn(inject_api_version))
}

/// Middleware that adds API versioning headers to all responses.
async fn inject_api_version(
    request: axum::http::Request<Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-api-version", axum::http::HeaderValue::from_static("1"));
    response
}

// -------------------------------------------------------------------------
/// Serve the OpenAPI 3.0 specification.
async fn openapi_spec() -> impl IntoResponse {
    let mut registry = crate::routes::route_metadata::RouteRegistry::new();
    registry.register_kernel_routes();
    Json(registry.to_openapi_json())
}

// Helper functions
// -------------------------------------------------------------------------

/// Build an error response with envelope.
fn error_response(status: StatusCode, message: &str) -> (StatusCode, Json<ErrorEnvelope>) {
    (
        status,
        Json(ErrorEnvelope {
            error: message.to_string(),
            status: status.as_u16(),
        }),
    )
}

/// Clamp per_page to a sane range.
fn clamp_per_page(per_page: i64) -> i64 {
    per_page.clamp(1, 100)
}

// -------------------------------------------------------------------------
// Search endpoint
// -------------------------------------------------------------------------

/// Search across all content.
///
/// `GET /api/v1/search?q=rust&page=1&per_page=25`
async fn search(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let per_page = clamp_per_page(params.per_page);
    let page = params.page.max(1);

    let query = params.q.as_deref().unwrap_or("");
    if query.is_empty() {
        return Json(ListEnvelope {
            data: Vec::<serde_json::Value>::new(),
            total: 0,
            page,
            per_page,
        })
        .into_response();
    }

    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let offset = (page - 1) * per_page;
    match state
        .search()
        .search(query, &[LIVE_STAGE_ID], user_id, per_page, offset)
        .await
    {
        Ok(results) => {
            let data: Vec<serde_json::Value> = results
                .results
                .into_iter()
                .map(|item| {
                    serde_json::json!({
                        "id": item.id,
                        "title": item.title,
                        "type": item.item_type,
                        "snippet": item.snippet,
                    })
                })
                .collect();

            Json(ListEnvelope {
                data,
                total: results.total as u64,
                page,
                per_page,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "search failed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Search failed").into_response()
        }
    }
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
    let session_user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    let Some(requester_id) = session_user_id else {
        return error_response(StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    };

    // Determine which user to export
    let target_user_id = if let Some(uid_str) = params.get("user_id") {
        let Ok(uid) = Uuid::parse_str(uid_str) else {
            return error_response(StatusCode::BAD_REQUEST, "Invalid user_id").into_response();
        };

        // Only admins can export other users' data
        if uid != requester_id {
            let requester = crate::models::User::find_by_id(state.db(), requester_id).await;
            if !requester
                .as_ref()
                .is_ok_and(|u| u.as_ref().is_some_and(|u| u.is_admin))
            {
                return error_response(StatusCode::FORBIDDEN, "Admin access required")
                    .into_response();
            }
        }
        uid
    } else {
        requester_id
    };

    // Load user profile
    let user = match crate::models::User::find_by_id(state.db(), target_user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return error_response(StatusCode::NOT_FOUND, "User not found").into_response();
        }
        Err(_) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
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
/// `GET /api/v1/items/autocomplete?type=article&q=rust&limit=10`
///
/// Returns `[{"id": "uuid", "title": "..."}]` for items matching the query.
async fn autocomplete_items(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let item_type = params.get("type").map(String::as_str).unwrap_or("");
    let query = params.get("q").map(String::as_str).unwrap_or("");
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(10)
        .min(50);

    if item_type.is_empty() || query.is_empty() {
        return Json(serde_json::json!([])).into_response();
    }

    // Search by title using ILIKE for case-insensitive prefix matching
    let pattern = format!("{}%", query.replace('%', "\\%").replace('_', "\\_"));

    match sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, title FROM item WHERE type = $1 AND title ILIKE $2 AND status = 1 ORDER BY title LIMIT $3",
    )
    .bind(item_type)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(state.db())
    .await
    {
        Ok(rows) => {
            let results: Vec<serde_json::Value> = rows
                .iter()
                .map(|(id, title)| {
                    serde_json::json!({
                        "id": id,
                        "title": title
                    })
                })
                .collect();
            Json(serde_json::Value::Array(results)).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "autocomplete query failed");
            Json(serde_json::json!([])).into_response()
        }
    }
}

// -------------------------------------------------------------------------
// Media browse endpoint
// -------------------------------------------------------------------------

/// Response envelope for the media browse API.
#[derive(Debug, Serialize)]
struct MediaBrowseResponse {
    /// Media items for the current page.
    items: Vec<MediaItem>,
    /// Total number of matching files.
    total: i64,
    /// Current page number (1-based).
    page: i64,
    /// Items per page.
    page_size: i64,
}

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
/// `GET /api/v1/media/browse?page=1&page_size=24&type=image&q=logo&sort=newest`
///
/// Query parameters:
/// - `page` — page number (default 1)
/// - `page_size` — items per page (default 24, max 100)
/// - `type` — MIME type prefix filter: `image`, `document`, or `all` (default)
/// - `q` — case-insensitive filename search
/// - `sort` — `newest` (default), `oldest`, `name`, `size`
async fn browse_media(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    // Require authentication
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    if user_id.is_none() {
        return error_response(StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    }

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let page_size: i64 = params
        .get("page_size")
        .and_then(|p| p.parse().ok())
        .unwrap_or(24)
        .clamp(1, 100);
    let sort = params.get("sort").map(String::as_str).unwrap_or("newest");
    let search = params.get("q").map(String::as_str);

    // Map the `type` parameter to a MIME prefix
    let mime_prefix: Option<&str> = match params.get("type").map(String::as_str) {
        Some("image") => Some("image/"),
        Some("document") => Some("application/"),
        _ => None,
    };

    let offset = (page - 1) * page_size;

    let total = match state
        .files()
        .count_filtered_media(Some(FileStatus::Permanent), mime_prefix, search)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "failed to count media");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to browse media")
                .into_response();
        }
    };

    let files = match state
        .files()
        .list_filtered_media(
            Some(FileStatus::Permanent),
            mime_prefix,
            search,
            sort,
            page_size,
            offset,
        )
        .await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "failed to list media");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to browse media")
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

    Json(MediaBrowseResponse {
        items,
        total,
        page,
        page_size,
    })
    .into_response()
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
/// Returns the component registry for the Puck editor to initialize with.
async fn list_page_builder_components() -> impl IntoResponse {
    let registry = crate::content::page_builder_components::ComponentRegistry::new();
    Json(serde_json::json!({
        "data": registry.all(),
    }))
}
