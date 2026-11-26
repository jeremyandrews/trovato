//! Admin routes for URL redirect management (B164).
//!
//! Registered only while `trovato_redirects` is enabled; see
//! [`super::gated_plugin_routes`].

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::error::AppError;
use crate::form::csrf::generate_csrf_token;
use crate::pagination::{Page, PageParams};
use crate::services::redirect::Redirect as RedirectModel;
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_csrf,
    require_csrf_header, require_permission, require_permission_json,
};

/// The redirect status choices: code and English label, translated by the
/// select macro.
const STATUS_OPTIONS: &[(&str, &str)] = &[
    ("301", "301 Moved Permanently"),
    ("302", "302 Found"),
    ("303", "303 See Other"),
    ("307", "307 Temporary Redirect"),
    ("308", "308 Permanent Redirect"),
];

/// Default page size of the redirect JSON list.
const LIST_DEFAULT: i64 = 50;

/// Largest page size of the redirect JSON list.
const LIST_MAX: i64 = 500;

// =============================================================================
// Form data
// =============================================================================

#[derive(Debug, Deserialize)]
struct RedirectFormData {
    #[serde(rename = "_token")]
    token: String,
    source: String,
    destination: String,
    status_code: i16,
    language: Option<String>,
}

/// Redirect display struct for templates.
#[derive(Debug, Serialize)]
struct RedirectDisplay {
    id: uuid::Uuid,
    source: String,
    destination: String,
    status_code: i16,
    language: String,
    created_display: String,
}

// =============================================================================
// Handlers
// =============================================================================

/// List all redirects.
///
/// GET /admin/config/redirects
async fn list_redirects(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "view redirects").await {
        return redirect;
    }

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: i64 = 50;
    let offset = (page - 1) * per_page;

    let redirects = match RedirectModel::list_all(state.db(), per_page, offset).await {
        Ok(redirects) => redirects,
        Err(e) => {
            tracing::error!(error = %e, "failed to list redirects");
            return render_server_error("Failed to load redirects.");
        }
    };

    let total = RedirectModel::count_all(state.db()).await.unwrap_or(0);
    let total_pages = (total as f64 / per_page as f64).ceil() as i64;

    let redirects_display: Vec<RedirectDisplay> = redirects
        .into_iter()
        .map(|r| RedirectDisplay {
            id: r.id,
            source: r.source,
            destination: r.destination,
            status_code: r.status_code,
            language: r.language,
            created_display: chrono::DateTime::from_timestamp(r.created, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "Unknown".to_string()),
        })
        .collect();

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("redirects", &redirects_display);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("total_pages", &total_pages);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/config/redirects");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/redirects.html",
        context,
    )
    .await
}

/// Add redirect form.
///
/// GET /admin/config/redirects/add
async fn add_redirect_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer redirects").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("action", "/admin/config/redirects/add");
    context.insert("editing", &false);
    context.insert("status_options", STATUS_OPTIONS);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/config/redirects/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/redirect-form.html",
        context,
    )
    .await
}

/// Add redirect submit.
///
/// POST /admin/config/redirects/add
async fn add_redirect_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RedirectFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer redirects").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let language = form.language.unwrap_or_default();
    match RedirectModel::create(
        state.db(),
        &form.source,
        &form.destination,
        form.status_code,
        &language,
    )
    .await
    {
        Ok(_) => {
            if let Some(cache) = state.redirect_cache() {
                cache.clear();
            }
            Redirect::to("/admin/config/redirects").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create redirect");
            render_server_error(
                "Failed to create redirect. The source may be invalid, or a fragment was given.",
            )
        }
    }
}

/// Edit redirect form.
///
/// GET /admin/config/redirects/{id}/edit
async fn edit_redirect_form(
    State(state): State<AppState>,
    session: Session,
    Path(redirect_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer redirects").await {
        return redirect;
    }

    let redirect = match RedirectModel::find_by_id(state.db(), redirect_id).await {
        Ok(Some(redirect)) => redirect,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load redirect");
            return render_server_error("Failed to load redirect.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert(
        "action",
        &format!("/admin/config/redirects/{redirect_id}/edit"),
    );
    context.insert("editing", &true);
    context.insert("status_options", STATUS_OPTIONS);
    context.insert(
        "values",
        &serde_json::json!({
            "source": redirect.source,
            "destination": redirect.destination,
            "status_code": redirect.status_code,
            "language": redirect.language,
        }),
    );
    context.insert(
        "path",
        &format!("/admin/config/redirects/{redirect_id}/edit"),
    );

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/redirect-form.html",
        context,
    )
    .await
}

/// Edit redirect submit.
///
/// POST /admin/config/redirects/{id}/edit
async fn edit_redirect_submit(
    State(state): State<AppState>,
    session: Session,
    Path(redirect_id): Path<uuid::Uuid>,
    Form(form): Form<RedirectFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer redirects").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let language = form.language.unwrap_or_default();
    match RedirectModel::update(
        state.db(),
        redirect_id,
        &form.source,
        &form.destination,
        form.status_code,
        &language,
    )
    .await
    {
        Ok(Some(_)) => {
            if let Some(cache) = state.redirect_cache() {
                cache.clear();
            }
            Redirect::to("/admin/config/redirects").into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update redirect");
            render_server_error(
                "Failed to update redirect. The source may be invalid, or a fragment was given.",
            )
        }
    }
}

/// Delete redirect.
///
/// POST /admin/config/redirects/{id}/delete
async fn delete_redirect(
    State(state): State<AppState>,
    session: Session,
    Path(redirect_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer redirects").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match RedirectModel::delete(state.db(), redirect_id).await {
        Ok(true) => {
            if let Some(cache) = state.redirect_cache() {
                cache.clear();
            }
            tracing::info!(redirect_id = %redirect_id, "redirect deleted");
            Redirect::to("/admin/config/redirects").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete redirect");
            render_server_error("Failed to delete redirect.")
        }
    }
}

// =============================================================================
// Router
// =============================================================================

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/config/redirects", get(list_redirects))
        .route("/admin/config/redirects/add", get(add_redirect_form))
        .route("/admin/config/redirects/add", post(add_redirect_submit))
        .route("/admin/config/redirects/{id}/edit", get(edit_redirect_form))
        .route(
            "/admin/config/redirects/{id}/edit",
            post(edit_redirect_submit),
        )
        .route("/admin/config/redirects/{id}/delete", post(delete_redirect))
}

// =============================================================================
// JSON API
// =============================================================================

/// A redirect, as the JSON API answers it.
#[derive(Debug, Serialize)]
struct RedirectResponse {
    id: uuid::Uuid,
    source: String,
    destination: String,
    status_code: i16,
    language: String,
}

impl From<RedirectModel> for RedirectResponse {
    fn from(r: RedirectModel) -> Self {
        Self {
            id: r.id,
            source: r.source,
            destination: r.destination,
            status_code: r.status_code,
            language: r.language,
        }
    }
}

/// A redirect create or update request body. `source` is stored normalized
/// (B164): sorted, canonically percent-encoded query parameters, and the
/// response echoes it back as stored rather than as typed.
#[derive(Debug, Deserialize)]
struct RedirectRequest {
    source: String,
    destination: String,
    status_code: i16,
    #[serde(default)]
    language: String,
}

async fn api_list_redirects(
    State(state): State<AppState>,
    session: Session,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<RedirectResponse>>, AppError> {
    require_permission_json(&state, &session, "view redirects").await?;

    let window = paging.window(LIST_DEFAULT, LIST_MAX);
    let redirects = RedirectModel::list_all(state.db(), window.per_page, window.offset)
        .await
        .map_err(AppError::internal)?;
    let total = RedirectModel::count_all(state.db())
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::new(
        redirects.into_iter().map(RedirectResponse::from).collect(),
        total.max(0) as u64,
        window,
    )))
}

async fn api_get_redirect(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<RedirectResponse>, AppError> {
    require_permission_json(&state, &session, "view redirects").await?;

    let redirect = RedirectModel::find_by_id(state.db(), id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found_id("redirect", id.to_string()))?;

    Ok(Json(redirect.into()))
}

async fn api_create_redirect(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(input): Json<RedirectRequest>,
) -> Result<(StatusCode, Json<RedirectResponse>), AppError> {
    require_permission_json(&state, &session, "administer redirects").await?;
    require_csrf_header(&session, &headers).await?;

    let redirect = RedirectModel::create(
        state.db(),
        &input.source,
        &input.destination,
        input.status_code,
        &input.language,
    )
    .await
    .map_err(|e| AppError::with_status(StatusCode::BAD_REQUEST, e.to_string()))?;

    if let Some(cache) = state.redirect_cache() {
        cache.clear();
    }

    Ok((StatusCode::CREATED, Json(redirect.into())))
}

async fn api_update_redirect(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<RedirectRequest>,
) -> Result<Json<RedirectResponse>, AppError> {
    require_permission_json(&state, &session, "administer redirects").await?;
    require_csrf_header(&session, &headers).await?;

    let redirect = RedirectModel::update(
        state.db(),
        id,
        &input.source,
        &input.destination,
        input.status_code,
        &input.language,
    )
    .await
    .map_err(|e| AppError::with_status(StatusCode::BAD_REQUEST, e.to_string()))?
    .ok_or_else(|| AppError::not_found_id("redirect", id.to_string()))?;

    if let Some(cache) = state.redirect_cache() {
        cache.clear();
    }

    Ok(Json(redirect.into()))
}

async fn api_delete_redirect(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, AppError> {
    require_permission_json(&state, &session, "administer redirects").await?;
    require_csrf_header(&session, &headers).await?;

    let deleted = RedirectModel::delete(state.db(), id)
        .await
        .map_err(AppError::internal)?;

    if !deleted {
        return Err(AppError::not_found_id("redirect", id.to_string()));
    }

    if let Some(cache) = state.redirect_cache() {
        cache.clear();
    }

    Ok(StatusCode::NO_CONTENT)
}

/// The redirect JSON API, under `/api/v1` ([`super::api`]), served while
/// `trovato_redirects` is enabled. Writes take `administer redirects` and the
/// `X-CSRF-Token` header; reads take `view redirects`.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("redirects")
        .get(
            "/redirects",
            api_list_redirects,
            doc("List redirects").paged(LIST_DEFAULT, LIST_MAX),
        )
        .post(
            "/redirect",
            api_create_redirect,
            doc(
                "Create a redirect; a source with a query matches only that \
                 query, normalized (sorted, canonically percent-encoded)",
            ),
        )
        .get("/redirect/{id}", api_get_redirect, doc("Get a redirect"))
        .put(
            "/redirect/{id}",
            api_update_redirect,
            doc("Update a redirect"),
        )
        .delete(
            "/redirect/{id}",
            api_delete_redirect,
            doc("Delete a redirect"),
        )
        .plugin("trovato_redirects")
}
