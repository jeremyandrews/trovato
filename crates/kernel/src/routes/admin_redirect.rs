//! Admin routes for URL redirect management (B164).
//!
//! Registered only while `trovato_redirects` is enabled; see
//! [`super::gated_plugin_routes`]. Every form here is built and checked by
//! the form pipeline (`routes::admin_form`): the add and edit form, and the
//! confirmation that deletes a redirect.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use axum::{Form as PostedBody, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::error::AppError;
use crate::form::{Form, FormElement, ValidationError};
use crate::models::User;
use crate::pagination::{Page, PageParams};
use crate::services::redirect::Redirect as RedirectModel;
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    render_admin_template, render_error, render_not_found, render_server_error,
    require_csrf_header, require_permission, require_permission_json,
};

/// The permission that opens the redirect list.
const VIEW: &str = "view redirects";

/// The permission every redirect form takes.
const ADMINISTER: &str = "administer redirects";

/// The redirect list, under which every redirect screen hangs.
const BASE: &str = "/admin/config/redirects";

/// The add and edit form.
const REDIRECT_FORM_ID: &str = "redirect_form";

/// The form that confirms deleting a redirect.
const DELETE_FORM_ID: &str = "redirect_delete_form";

/// The redirect status choices: code and English label, translated by the
/// select template.
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

/// What the redirect form's fields hold.
#[derive(Debug)]
struct RedirectValues {
    source: String,
    destination: String,
    status_code: String,
    language: String,
}

impl Default for RedirectValues {
    fn default() -> Self {
        Self {
            source: String::new(),
            destination: String::new(),
            status_code: "301".to_string(),
            language: String::new(),
        }
    }
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
    if let Err(redirect) = require_permission(&state, &session, VIEW).await {
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

    let mut context = tera::Context::new();
    context.insert("redirects", &redirects_display);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("total_pages", &total_pages);
    context.insert("path", BASE);

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/redirects.html",
        context,
    )
    .await
}

// =============================================================================
// Forms
// =============================================================================

/// A text field for a path or a code: no spell checking, no capitals.
fn plain_text() -> FormElement {
    FormElement::textfield()
        .attribute("spellcheck", "false")
        .attribute("autocapitalize", "none")
}

/// The add and edit form: `action` is where it posts, `values` what its
/// fields hold, `editing` whether it saves an existing redirect, and
/// `cancel` the link beside its button.
fn redirect_form(action: &str, values: &RedirectValues, editing: bool, cancel: String) -> Form {
    let status_options = STATUS_OPTIONS
        .iter()
        .map(|(code, label)| ((*code).to_string(), (*label).to_string()))
        .collect();
    Form::new(REDIRECT_FORM_ID)
        .action(action)
        .element(
            "source",
            plain_text()
                .title("Source path")
                .description(
                    "The old path to redirect from, with an optional query string, for \
                     example /article.php?sid=123. A source with a query matches only a \
                     request with that query; a source with no query matches any query. \
                     Parameter order and percent-encoding are normalized, so the source is \
                     shown back as it is stored.",
                )
                .default_value(values.source.clone())
                .required()
                .weight(0),
        )
        .element(
            "destination",
            plain_text()
                .title("Destination")
                .description(
                    "A path on this site, such as /blog/the-new-page, or an http or https \
                     address.",
                )
                .default_value(values.destination.clone())
                .required()
                .weight(1),
        )
        .element(
            "status_code",
            FormElement::select(status_options)
                .title("Redirect type")
                .default_value(values.status_code.clone())
                .required()
                .weight(2),
        )
        .element(
            "language",
            plain_text()
                .title("Language")
                .description("A language code, such as en. Leave blank to match every language.")
                .default_value(values.language.clone())
                .weight(3),
        )
        .element(
            "op",
            FormElement::submit(if editing {
                "Save changes"
            } else {
                "Create redirect"
            })
            .suffix(cancel)
            .weight(10),
        )
}

/// The confirmation form that deletes the redirect `id`.
fn delete_form_for(id: uuid::Uuid, cancel: String) -> Form {
    Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{id}/delete"))
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .suffix(cancel)
                .weight(10),
        )
}

/// A Cancel link back to the redirect list, in `user`'s admin language on
/// `path`.
async fn cancel_link(state: &AppState, user: &User, path: &str) -> String {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, path).await;
    format!(
        r#"<a href="{BASE}" class="button button--secondary">{}</a>"#,
        text.html("Cancel", &[])
    )
}

/// What the redirect form posted, its status code when that is one of the
/// choices, and the errors that keep it from saving.
fn read_redirect(posted: &PostedForm) -> (RedirectValues, Option<i16>, Vec<ValidationError>) {
    let values = RedirectValues {
        source: posted.get("source").unwrap_or_default().to_string(),
        destination: posted.get("destination").unwrap_or_default().to_string(),
        status_code: posted.get("status_code").unwrap_or_default().to_string(),
        language: posted.get("language").unwrap_or_default().to_string(),
    };
    let mut errors = Vec::new();
    if values.source.trim().is_empty() {
        errors.push(ValidationError::field("source", "Source path is required."));
    }
    if values.destination.trim().is_empty() {
        errors.push(ValidationError::field(
            "destination",
            "Destination is required.",
        ));
    }
    let status_code = STATUS_OPTIONS
        .iter()
        .any(|(code, _)| *code == values.status_code)
        .then(|| values.status_code.parse::<i16>().ok())
        .flatten();
    if status_code.is_none() {
        errors.push(ValidationError::field(
            "status_code",
            "Choose a redirect type.",
        ));
    }
    (values, status_code, errors)
}

/// Render the add form, or the edit form of the redirect `editing`.
async fn render_redirect_form(
    state: &AppState,
    session: &Session,
    user: &User,
    editing: Option<uuid::Uuid>,
    values: &RedirectValues,
    errors: &[ValidationError],
) -> Response {
    let action = match editing {
        Some(id) => format!("{BASE}/{id}/edit"),
        None => format!("{BASE}/add"),
    };
    let cancel = cancel_link(state, user, &action).await;
    let form_html = match admin_form::render(
        state,
        session,
        user,
        redirect_form(&action, values, editing.is_some(), cancel),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("editing", &editing.is_some());
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/redirect-form.html", context).await
}

/// The redirect `id`, or the page that says why it is not there.
async fn load_redirect(state: &AppState, id: uuid::Uuid) -> Result<RedirectModel, Response> {
    match RedirectModel::find_by_id(state.db(), id).await {
        Ok(Some(redirect)) => Ok(redirect),
        Ok(None) => Err(render_not_found()),
        Err(e) => {
            tracing::error!(error = %e, "failed to load redirect");
            Err(render_server_error("Failed to load redirect."))
        }
    }
}

/// Add redirect form.
///
/// GET /admin/config/redirects/add
async fn add_redirect_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_redirect_form(
        &state,
        &session,
        &user,
        None,
        &RedirectValues::default(),
        &[],
    )
    .await
}

/// Add redirect submit.
///
/// POST /admin/config/redirects/add
async fn add_redirect_submit(
    State(state): State<AppState>,
    session: Session,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (values, status_code, errors) = read_redirect(&posted);
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, REDIRECT_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_redirect_form(&state, &session, &user, None, &values, &errors).await;
            }
            Err(Err(response)) => return response,
        };
    let Some(status_code) = status_code else {
        return render_not_found();
    };

    match state
        .redirects()
        .create(
            &values.source,
            &values.destination,
            status_code,
            &values.language,
        )
        .await
    {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to(BASE).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create redirect");
            render_server_error(&format!("Failed to create redirect: {e}"))
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
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let redirect = match load_redirect(&state, redirect_id).await {
        Ok(redirect) => redirect,
        Err(response) => return response,
    };
    let values = RedirectValues {
        source: redirect.source,
        destination: redirect.destination,
        status_code: redirect.status_code.to_string(),
        language: redirect.language,
    };
    render_redirect_form(&state, &session, &user, Some(redirect_id), &values, &[]).await
}

/// Edit redirect submit.
///
/// POST /admin/config/redirects/{id}/edit
async fn edit_redirect_submit(
    State(state): State<AppState>,
    session: Session,
    Path(redirect_id): Path<uuid::Uuid>,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (values, status_code, errors) = read_redirect(&posted);
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, REDIRECT_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_redirect_form(
                    &state,
                    &session,
                    &user,
                    Some(redirect_id),
                    &values,
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };
    let Some(status_code) = status_code else {
        return render_not_found();
    };

    match state
        .redirects()
        .update(
            redirect_id,
            &values.source,
            &values.destination,
            status_code,
            &values.language,
        )
        .await
    {
        Ok(Some(_)) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to(BASE).into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update redirect");
            render_server_error(&format!("Failed to update redirect: {e}"))
        }
    }
}

/// Confirm deleting a redirect.
///
/// GET /admin/config/redirects/{id}/delete
async fn delete_redirect_form(
    State(state): State<AppState>,
    session: Session,
    Path(redirect_id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let redirect = match load_redirect(&state, redirect_id).await {
        Ok(redirect) => redirect,
        Err(response) => return response,
    };
    let path = format!("{BASE}/{redirect_id}/delete");
    let cancel = cancel_link(&state, &user, &path).await;
    let form_html = match admin_form::render(
        &state,
        &session,
        &user,
        delete_form_for(redirect_id, cancel),
        &[],
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("source", &redirect.source);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(&state, &viewer, "admin/redirect-delete.html", context).await
}

/// Delete redirect.
///
/// POST /admin/config/redirects/{id}/delete
async fn delete_redirect(
    State(state): State<AppState>,
    session: Session,
    Path(redirect_id): Path<uuid::Uuid>,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, DELETE_FORM_ID, Vec::new()).await
        {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return render_error("The form has expired. Reload the page and try again.");
            }
            Err(Err(response)) => return response,
        };

    match state.redirects().delete(redirect_id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(redirect_id = %redirect_id, "redirect deleted");
            Redirect::to(BASE).into_response()
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
        .route(
            "/admin/config/redirects/add",
            get(add_redirect_form).post(add_redirect_submit),
        )
        .route(
            "/admin/config/redirects/{id}/edit",
            get(edit_redirect_form).post(edit_redirect_submit),
        )
        .route(
            "/admin/config/redirects/{id}/delete",
            get(delete_redirect_form).post(delete_redirect),
        )
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

    let redirect = state
        .redirects()
        .create(
            &input.source,
            &input.destination,
            input.status_code,
            &input.language,
        )
        .await
        .map_err(|e| AppError::with_status(StatusCode::BAD_REQUEST, e.to_string()))?;

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

    let redirect = state
        .redirects()
        .update(
            id,
            &input.source,
            &input.destination,
            input.status_code,
            &input.language,
        )
        .await
        .map_err(|e| AppError::with_status(StatusCode::BAD_REQUEST, e.to_string()))?
        .ok_or_else(|| AppError::not_found_id("redirect", id.to_string()))?;

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

    let deleted = state
        .redirects()
        .delete(id)
        .await
        .map_err(AppError::internal)?;

    if !deleted {
        return Err(AppError::not_found_id("redirect", id.to_string()));
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
