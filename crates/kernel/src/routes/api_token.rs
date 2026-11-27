//! API token management routes.
//!
//! All endpoints require an authenticated session (cookie or existing Bearer token).
//!
//! `/user/tokens` is the page a person manages their own tokens on. It renders
//! the list and the create form, `user_api_token_create`, built through the
//! forms pipeline; creating and revoking go through the JSON endpoints below,
//! called by `static/js/api-tokens.js` with the page's CSRF token, so the page
//! and an API client use one contract. Without script the create form posts
//! to `POST /user/tokens`, checked through the pipeline. The raw token exists
//! only in the create response: the JSON answer, or the page answering that
//! post, which shows it once and is never cached.

use axum::http::header;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::error::AppError;
use crate::models::api_token::{ApiToken, MAX_TOKENS_PER_USER};
use crate::routes::helpers::{require_csrf_header, require_login_json};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    pub name: String,
    /// Optional expiration in days from now. If omitted, the token never expires.
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CreateTokenResponse {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TokenListItem {
    pub id: Uuid,
    pub name: String,
    pub created: i64,
    pub last_used: Option<i64>,
    pub expires_at: Option<i64>,
}

/// POST /api/tokens — Create a new API token.
async fn create_token(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), AppError> {
    let user_id = require_login_json(&state, &session).await?.id;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;

    let name = body.name.trim();
    check_new_token(&state, user_id, name)
        .await?
        .map_or(Ok(()), |message| Err(AppError::bad_request(message)))?;

    let expires_at = body
        .expires_in_days
        .map(|days| Utc::now() + Duration::days(i64::from(days)));

    let (token_record, raw_token) =
        crate::services::api_token::create(&state, user_id, name, expires_at)
            .await
            .map_err(|e| AppError::internal_ctx(e, "create API token"))?;

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            id: token_record.id,
            name: token_record.name,
            token: raw_token,
            expires_at: token_record.expires_at.map(|t| t.timestamp()),
        }),
    ))
}

/// Why a token named `name` cannot be created for `user_id`, if it cannot:
/// the name's length, or the per-user limit. Shared by the JSON endpoint and
/// the page's form.
async fn check_new_token(
    state: &AppState,
    user_id: Uuid,
    name: &str,
) -> Result<Option<String>, AppError> {
    if name.is_empty() || name.len() > 255 {
        return Ok(Some("Token name must be 1-255 characters".to_string()));
    }

    // Enforce per-user token limit
    let count = ApiToken::count_for_user(state.db(), user_id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "count API tokens"))?;

    if count >= MAX_TOKENS_PER_USER {
        return Ok(Some(format!(
            "Maximum of {MAX_TOKENS_PER_USER} tokens per user"
        )));
    }
    Ok(None)
}

/// GET /api/tokens — List the current user's tokens.
async fn list_tokens(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(paging): axum::extract::Query<crate::pagination::PageParams>,
) -> Result<Json<crate::pagination::Page<TokenListItem>>, AppError> {
    let user_id = require_login_json(&state, &session).await?.id;

    let tokens = ApiToken::list_for_user(state.db(), user_id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "list API tokens"))?;

    let items: Vec<TokenListItem> = tokens
        .into_iter()
        .map(|t| TokenListItem {
            id: t.id,
            name: t.name,
            created: t.created.timestamp(),
            last_used: t.last_used.map(|ts| ts.timestamp()),
            expires_at: t.expires_at.map(|ts| ts.timestamp()),
        })
        .collect();

    Ok(Json(crate::pagination::Page::slice(
        items,
        paging.window(100, 100),
    )))
}

/// DELETE /api/tokens/{id} — Revoke a token.
async fn delete_token(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let user_id = require_login_json(&state, &session).await?.id;

    // Verify CSRF token from header
    require_csrf_header(&session, &headers)
        .await
        .map_err(|_| AppError::forbidden("Invalid or missing CSRF token"))?;

    let deleted = crate::services::api_token::revoke(&state, id, user_id)
        .await
        .map_err(|e| AppError::internal_ctx(e, "delete API token"))?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found_id("token", id))
    }
}

/// One token as the page lists it, with its times already formatted.
#[derive(Debug, Serialize)]
struct TokenRow {
    id: Uuid,
    name: String,
    created: String,
    last_used: Option<String>,
    expires_at: Option<String>,
}

fn page_time(t: chrono::DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// The create form on the token page, `user_api_token_create`, showing
/// `values` (name, days) when it is shown again.
fn build_create_form(name: &str, days: &str) -> crate::form::Form {
    use crate::form::{Form, FormElement};
    Form::new("user_api_token_create")
        .action("/user/tokens")
        .element(
            "name",
            FormElement::textfield()
                .title("Name")
                .required()
                .max_length(255)
                .placeholder("e.g. Deploy script")
                .default_value(name)
                .weight(0),
        )
        .element(
            "expires_in_days",
            FormElement::input("number")
                .title("Expires after (days)")
                .placeholder("Leave empty for a token that does not expire")
                .attribute("min", "1")
                .attribute("max", "3650")
                .default_value(days)
                .weight(1),
        )
        .element("op", FormElement::submit("Create token").weight(100))
}

/// GET /user/tokens — the signed-in user's API tokens, and the forms to create
/// and revoke them.
async fn tokens_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match crate::routes::helpers::require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    render_tokens_page(&state, &session, &user, TokensPage::default()).await
}

/// What the token page shows besides the list: the create form's values
/// and errors, or the token just created.
#[derive(Default)]
struct TokensPage<'a> {
    name: &'a str,
    days: &'a str,
    errors: Vec<crate::form::ValidationError>,
    created: Option<String>,
}

/// Render the token page for `user`.
async fn render_tokens_page(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    page: TokensPage<'_>,
) -> Response {
    let csrf_token = crate::form::csrf::generate_csrf_token(session).await;
    let tokens = match ApiToken::list_for_user(state.db(), user.id).await {
        Ok(tokens) => tokens,
        Err(e) => {
            tracing::error!(error = %e, "failed to list API tokens for the token page");
            return crate::routes::helpers::render_server_error("Could not load your API tokens.");
        }
    };
    let rows: Vec<TokenRow> = tokens
        .into_iter()
        .map(|t| TokenRow {
            id: t.id,
            name: t.name,
            created: page_time(t.created),
            last_used: t.last_used.map(page_time),
            expires_at: t.expires_at.map(page_time),
        })
        .collect();

    let mut context = tera::Context::new();
    if rows.len() < MAX_TOKENS_PER_USER as usize {
        let viewer = crate::routes::helpers::user_context_for(state, user).await;
        match super::admin_form::render_as(
            state,
            session,
            &viewer,
            build_create_form(page.name, page.days),
            &page.errors,
        )
        .await
        {
            Ok(html) => context.insert("create_form", &html),
            Err(refusal) => return refusal,
        }
    }
    context.insert("csrf_token", &csrf_token);
    context.insert("user", user);
    context.insert("tokens", &rows);
    context.insert("max_tokens", &MAX_TOKENS_PER_USER);
    if let Some(created) = &page.created {
        context.insert("created_token", created);
    }
    crate::routes::helpers::inject_site_context(state, session, &mut context, "/user/tokens").await;

    let status = if page.errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    match state.theme().tera().render("user/tokens.html", &context) {
        // A page holding a raw token must not be kept by any cache.
        Ok(html) => (status, [(header::CACHE_CONTROL, "no-store")], Html(html)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the token page");
            crate::routes::helpers::render_server_error("Could not render the token page.")
        }
    }
}

/// POST /user/tokens — the create form, posted without script.
///
/// Checked through the forms pipeline, then the same checks as the JSON
/// endpoint; the page that answers shows the new token once.
async fn create_token_form(
    State(state): State<AppState>,
    session: Session,
    axum::Form(pairs): axum::Form<Vec<(String, String)>>,
) -> Response {
    let posted = crate::content::submission::PostedForm(pairs);
    let user = match crate::routes::helpers::require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let name = posted.get("name").unwrap_or_default().trim();
    let days = posted.get("expires_in_days").unwrap_or_default().trim();
    let mut errors = Vec::new();
    match check_new_token(&state, user.id, name).await {
        Ok(Some(message)) => errors.push(crate::form::ValidationError::form(message)),
        Ok(None) => {}
        Err(e) => return e.into_response(),
    }
    let expires_in_days = match days {
        "" => None,
        days => match days.parse::<u32>() {
            Ok(days) if (1..=3650).contains(&days) => Some(days),
            _ => {
                errors.push(crate::form::ValidationError::field(
                    "expires_in_days",
                    "Expires after must be a whole number of days from 1 to 3650.",
                ));
                None
            }
        },
    };
    let viewer = crate::routes::helpers::user_context_for(&state, &user).await;
    let form_state = match super::admin_form::check_as(
        &state,
        &session,
        &viewer,
        &posted,
        "user_api_token_create",
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Err(refusal)) => return refusal,
        Err(Ok(errors)) => {
            let page = TokensPage {
                name,
                days,
                errors,
                created: None,
            };
            return render_tokens_page(&state, &session, &user, page).await;
        }
    };

    let expires_at = expires_in_days.map(|days| Utc::now() + Duration::days(i64::from(days)));
    let raw_token =
        match crate::services::api_token::create(&state, user.id, name, expires_at).await {
            Ok((_, raw_token)) => raw_token,
            Err(e) => return AppError::internal_ctx(e, "create API token").into_response(),
        };
    state
        .forms()
        .submitted(
            &form_state,
            &posted,
            None,
            &super::item_form::tap_state(&state, &viewer),
        )
        .await;
    let page = TokensPage {
        created: Some(raw_token),
        ..Default::default()
    };
    render_tokens_page(&state, &session, &user, page).await
}

pub fn router() -> Router<AppState> {
    Router::new().route("/user/tokens", get(tokens_page).post(create_token_form))
}

/// The API token JSON API, under `/api/v1` ([`super::api`]).
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    super::api::ApiRoutes::new("tokens")
        .post(
            "/tokens",
            create_token,
            doc("Create an API token for the caller (`X-CSRF-Token`)").alias(),
        )
        .get(
            "/tokens",
            list_tokens,
            doc("List the caller's API tokens").paged(100, 100).alias(),
        )
        .delete(
            "/tokens/{id}",
            delete_token,
            doc("Revoke one of the caller's API tokens (`X-CSRF-Token`)").alias(),
        )
}
