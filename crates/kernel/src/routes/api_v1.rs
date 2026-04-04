//! Versioned REST API (v1) for Ritrovo conferences.
//!
//! Provides a stable, paginated JSON API with envelope responses.
//! All list endpoints return `{ data, total, page, per_page }`.
//! Single-resource endpoints return `{ data }`.
//! Error endpoints return `{ error, status }`.

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tower_sessions::Session;
use uuid::Uuid;

use crate::gather::{FilterValue, QueryContext};
use crate::middleware::language::ResolvedLanguage;
use crate::models::Subscription;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::auth::SESSION_USER_ID;
use crate::routes::helpers::require_csrf_header;
use crate::state::AppState;

// -------------------------------------------------------------------------
// Gather query ID constants
// -------------------------------------------------------------------------

/// Gather query ID for upcoming conference listings.
const QUERY_UPCOMING_CONFERENCES: &str = "upcoming_conferences";

/// Gather query ID for conferences filtered by topic.
const QUERY_CONFERENCES_BY_TOPIC: &str = "conferences_by_topic";

/// Gather query ID for speaker listings.
const QUERY_SPEAKERS: &str = "speakers";

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

/// Single-resource envelope.
#[derive(Debug, Serialize)]
struct DataEnvelope<T: Serialize> {
    data: T,
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
#[derive(Debug, Deserialize)]
struct ListParams {
    #[serde(default = "default_page")]
    page: i64,
    #[serde(default = "default_per_page")]
    per_page: i64,
    topic: Option<String>,
    country: Option<String>,
    online: Option<String>,
    lang: Option<String>,
    stage: Option<String>,
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
        .route("/api/v1/conferences", get(list_conferences))
        .route("/api/v1/conferences/{id}", get(get_conference))
        .route("/api/v1/topics", get(list_topics))
        .route(
            "/api/v1/topics/{id}/conferences",
            get(list_topic_conferences),
        )
        .route("/api/v1/search", get(search))
        .route("/api/v1/speakers", get(list_speakers))
        .route("/api/v1/speakers/{id}", get(get_speaker))
        .route(
            "/api/v1/conferences/{id}/subscribe",
            post(subscribe).delete(unsubscribe),
        )
        .route("/api/v1/user/export", get(export_user_data))
        .route("/api/v1/items/autocomplete", get(autocomplete_items))
        .route("/api/openapi.json", get(openapi_spec))
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
// Conference endpoints
// -------------------------------------------------------------------------

/// List conferences with optional filtering and pagination.
///
/// `GET /api/v1/conferences?page=1&per_page=25&topic=...&country=...`
async fn list_conferences(
    State(state): State<AppState>,
    session: Session,
    Extension(lang): Extension<ResolvedLanguage>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let per_page = clamp_per_page(params.per_page);
    let page = params.page.max(1);

    // Try the "upcoming_conferences" gather query if available
    let query_id = QUERY_UPCOMING_CONFERENCES;
    if state.gather().get_query(query_id).is_some() {
        let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
        let mut url_args = HashMap::new();
        if let Some(ref topic) = params.topic {
            url_args.insert("topic".to_string(), topic.clone());
        }
        if let Some(ref country) = params.country {
            url_args.insert("country".to_string(), country.clone());
        }
        let language = if lang.0 != state.default_language() {
            Some(lang.0.clone())
        } else {
            None
        };
        let context = QueryContext {
            current_user_id: user_id,
            url_args,
            language,
        };

        let exposed_filters = build_exposed_filters(&params);

        let stage_id = params
            .stage
            .as_deref()
            .and_then(|s| s.parse::<Uuid>().ok())
            .unwrap_or(LIVE_STAGE_ID);

        #[allow(clippy::cast_possible_truncation)]
        let page_u32 = page as u32;

        match state
            .gather()
            .execute(query_id, page_u32, exposed_filters, stage_id, &context)
            .await
        {
            Ok(result) => {
                return Json(ListEnvelope {
                    data: result.items,
                    total: result.total,
                    page,
                    per_page,
                })
                .into_response();
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to execute gather query");
            }
        }
    }

    // Fallback: use ItemService directly
    let offset = (page - 1) * per_page;
    match state
        .items()
        .list_filtered(Some("conference"), Some(1), None, per_page, offset)
        .await
    {
        Ok((items, total)) => {
            let data: Vec<serde_json::Value> = items
                .into_iter()
                .map(|item| {
                    serde_json::json!({
                        "id": item.id,
                        "title": item.title,
                        "type": item.item_type,
                        "status": item.status,
                        "created": item.created,
                        "changed": item.changed,
                        "fields": item.fields,
                    })
                })
                .collect();

            let _ = &lang; // acknowledge language for future translation overlay
            Json(ListEnvelope {
                data,
                total: total as u64,
                page,
                per_page,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list conferences");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load conferences",
            )
            .into_response()
        }
    }
}

/// Get a single conference by ID.
///
/// `GET /api/v1/conferences/{id}`
async fn get_conference(
    State(state): State<AppState>,
    Extension(lang): Extension<ResolvedLanguage>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let mut item = match state.items().load(id).await {
        Ok(Some(item)) if item.item_type == "conference" => item,
        Ok(_) => {
            return error_response(StatusCode::NOT_FOUND, "Conference not found").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load conference");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load conference",
            )
            .into_response();
        }
    };

    // Translation overlay (shared with item::view_item)
    if lang.0 != state.default_language() {
        super::helpers::apply_translation_overlay(state.items(), &mut item, &lang.0).await;
    }

    Json(DataEnvelope {
        data: serde_json::json!({
            "id": item.id,
            "title": item.title,
            "type": item.item_type,
            "status": item.status,
            "created": item.created,
            "changed": item.changed,
            "fields": item.fields,
        }),
    })
    .into_response()
}

// -------------------------------------------------------------------------
// Topic endpoints
// -------------------------------------------------------------------------

/// List all topics (categories).
///
/// `GET /api/v1/topics`
///
/// Returns all categories in a single page (categories are a small bounded set).
async fn list_topics(State(state): State<AppState>) -> impl IntoResponse {
    match state.categories().list_categories().await {
        Ok(categories) => {
            let data: Vec<serde_json::Value> = categories
                .into_iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "label": c.label,
                        "description": c.description,
                    })
                })
                .collect();
            let total = data.len() as u64;
            Json(ListEnvelope {
                data,
                total,
                page: 1,
                per_page: total as i64,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list categories");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load topics")
                .into_response()
        }
    }
}

/// List conferences for a specific topic.
///
/// `GET /api/v1/topics/{id}/conferences`
async fn list_topic_conferences(
    State(state): State<AppState>,
    session: Session,
    Path(topic_id): Path<Uuid>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let per_page = clamp_per_page(params.per_page);
    let page = params.page.max(1);

    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let mut url_args = HashMap::new();
    url_args.insert("topic".to_string(), topic_id.to_string());

    let context = QueryContext {
        current_user_id: user_id,
        url_args,
        language: None,
    };

    let query_id = if state
        .gather()
        .get_query(QUERY_CONFERENCES_BY_TOPIC)
        .is_some()
    {
        QUERY_CONFERENCES_BY_TOPIC
    } else {
        QUERY_UPCOMING_CONFERENCES
    };

    let mut exposed_filters = HashMap::new();
    exposed_filters.insert(
        "field_topics".to_string(),
        FilterValue::String(topic_id.to_string()),
    );

    #[allow(clippy::cast_possible_truncation)]
    let page_u32 = page as u32;

    match state
        .gather()
        .execute(query_id, page_u32, exposed_filters, LIVE_STAGE_ID, &context)
        .await
    {
        Ok(result) => Json(ListEnvelope {
            data: result.items,
            total: result.total,
            page,
            per_page,
        })
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to execute topic query");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load topic conferences",
            )
            .into_response()
        }
    }
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
// Speaker endpoints
// -------------------------------------------------------------------------

/// List speakers.
///
/// `GET /api/v1/speakers?page=1&per_page=25`
async fn list_speakers(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let per_page = clamp_per_page(params.per_page);
    let page = params.page.max(1);

    if state.gather().get_query(QUERY_SPEAKERS).is_some() {
        let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
        let context = QueryContext {
            current_user_id: user_id,
            url_args: HashMap::new(),
            language: None,
        };

        #[allow(clippy::cast_possible_truncation)]
        let page_u32 = page as u32;

        match state
            .gather()
            .execute(
                QUERY_SPEAKERS,
                page_u32,
                HashMap::new(),
                LIVE_STAGE_ID,
                &context,
            )
            .await
        {
            Ok(result) => {
                return Json(ListEnvelope {
                    data: result.items,
                    total: result.total,
                    page,
                    per_page,
                })
                .into_response();
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to list speakers");
            }
        }
    }

    // Fallback: list speaker content type items
    let offset = (page - 1) * per_page;
    match state
        .items()
        .list_filtered(Some("speaker"), Some(1), None, per_page, offset)
        .await
    {
        Ok((items, total)) => {
            let data: Vec<serde_json::Value> = items
                .into_iter()
                .map(|item| {
                    serde_json::json!({
                        "id": item.id,
                        "title": item.title,
                        "fields": item.fields,
                    })
                })
                .collect();

            Json(ListEnvelope {
                data,
                total: total as u64,
                page,
                per_page,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list speakers");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load speakers")
                .into_response()
        }
    }
}

/// Get a single speaker by ID.
///
/// `GET /api/v1/speakers/{id}`
async fn get_speaker(State(state): State<AppState>, Path(id): Path<Uuid>) -> impl IntoResponse {
    match state.items().load(id).await {
        Ok(Some(item)) => Json(DataEnvelope {
            data: serde_json::json!({
                "id": item.id,
                "title": item.title,
                "type": item.item_type,
                "fields": item.fields,
                "created": item.created,
                "changed": item.changed,
            }),
        })
        .into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Speaker not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load speaker");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load speaker")
                .into_response()
        }
    }
}

// -------------------------------------------------------------------------
// Subscription endpoints
// -------------------------------------------------------------------------

/// Subscribe to a conference.
///
/// `POST /api/v1/conferences/{id}/subscribe`
async fn subscribe(
    State(state): State<AppState>,
    session: Session,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if let Err(resp) = require_csrf_header(&session, &headers).await {
        return resp.into_response();
    }
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let Some(user_id) = user_id else {
        return error_response(StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    };

    // Verify the conference exists
    match state.items().load(id).await {
        Ok(Some(item)) if item.item_type == "conference" => {}
        _ => {
            return error_response(StatusCode::NOT_FOUND, "Conference not found").into_response();
        }
    }

    match Subscription::subscribe(state.db(), user_id, id).await {
        Ok(()) => Json(DataEnvelope {
            data: serde_json::json!({ "subscribed": true, "item_id": id }),
        })
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to subscribe");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to subscribe").into_response()
        }
    }
}

/// Unsubscribe from a conference.
///
/// `DELETE /api/v1/conferences/{id}/subscribe`
async fn unsubscribe(
    State(state): State<AppState>,
    session: Session,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if let Err(resp) = require_csrf_header(&session, &headers).await {
        return resp.into_response();
    }
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    let Some(user_id) = user_id else {
        return error_response(StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    };

    match Subscription::unsubscribe(state.db(), user_id, id).await {
        Ok(_) => Json(DataEnvelope {
            data: serde_json::json!({ "subscribed": false, "item_id": id }),
        })
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to unsubscribe");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to unsubscribe")
                .into_response()
        }
    }
}

// -------------------------------------------------------------------------
// Internal helpers
// -------------------------------------------------------------------------

/// Build exposed filter values from query parameters.
fn build_exposed_filters(params: &ListParams) -> HashMap<String, FilterValue> {
    let mut filters = HashMap::new();

    if let Some(ref topic) = params.topic {
        filters.insert(
            "field_topics".to_string(),
            FilterValue::String(topic.clone()),
        );
    }

    if let Some(ref country) = params.country {
        filters.insert(
            "field_country".to_string(),
            FilterValue::String(country.clone()),
        );
    }

    if let Some(ref online) = params.online {
        filters.insert(
            "field_online".to_string(),
            FilterValue::String(online.clone()),
        );
    }

    filters
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

/// Get field names marked `personal_data: true` for a content type.
///
/// Returns an empty vec if the type is not found or has no PII fields.
/// Autocomplete endpoint for RecordReference fields.
///
/// `GET /api/v1/items/autocomplete?type=conference&q=rust&limit=10`
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
