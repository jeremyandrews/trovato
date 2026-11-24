//! Password reset by email.
//!
//! Two steps, each a page a browser completes and, for a client that sends
//! JSON, the same step answered in JSON:
//!
//! 1. `GET /user/password-reset` renders a form asking for an email address;
//!    `POST` mails a reset link if the address has an account, and answers
//!    every request the same way whether it does or not.
//! 2. The link, `GET /user/password-reset/{token}`, renders a set-password
//!    form; `POST` spends the token, sets the password, ends every session of
//!    the account and redirects to the login page.
//!
//! A `POST` whose `Content-Type` is `application/json`, and a `GET` of the link
//! that `Accept`s `application/json`, keep the JSON answers they always had.
//! Everything else is a browser and gets pages; their forms carry a CSRF token.

use axum::extract::{FromRequest, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Json, Router};
use serde::Deserialize;
use tower_sessions::Session;
use tracing::info;

use crate::error::AppError;
use crate::form::csrf::generate_csrf_token;
use crate::models::password_reset::PasswordResetToken;
use crate::routes::auth::SESSION_LOGIN_NOTICE;
use crate::routes::helpers::{JsonSuccess, render_server_error, require_csrf, validate_password};
use crate::state::AppState;
use crate::tap::UserContext;

/// The one sentence every reset request gets.
///
/// Known address, unknown address, over a limit or not: the same status and
/// the same words, so nothing about the answer says whether an account exists
/// or whether a limit was reached.
const GENERIC_ANSWER: &str = "If an account with that email exists, a reset link has been sent.";

/// What the login page says after a reset.
const RESET_DONE_NOTICE: &str = "Your password has been reset. Sign in with your new password.";

/// Password reset request (step 1: request reset), as JSON.
#[derive(Debug, Deserialize)]
pub struct RequestResetInput {
    pub email: String,
}

/// Password reset request (step 1), as the page's form.
#[derive(Debug, Deserialize)]
struct RequestResetForm {
    #[serde(rename = "_token")]
    token: String,
    email: String,
}

/// New password input (step 2: set new password), as JSON.
#[derive(Debug, Deserialize)]
pub struct SetPasswordInput {
    pub password: String,
}

/// New password (step 2), as the page's form.
#[derive(Debug, Deserialize)]
struct SetPasswordForm {
    #[serde(rename = "_token")]
    token: String,
    password: String,
    confirm_password: String,
}

/// Whether the request body is JSON.
fn sends_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
}

/// Whether the client asked for a JSON answer.
fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}

/// Render a reset page. `no_referrer` is set on every page whose URL carries
/// a token, so no referrer can carry the token off-site.
async fn render_page(
    state: &AppState,
    session: &Session,
    template: &str,
    status: StatusCode,
    mut context: tera::Context,
    no_referrer: bool,
) -> Response {
    super::helpers::inject_site_context(state, session, &mut context, "/user/password-reset").await;
    match state.theme().tera().render(template, &context) {
        Ok(html) if no_referrer => (
            status,
            [(header::REFERRER_POLICY, "no-referrer")],
            Html(html),
        )
            .into_response(),
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template, "failed to render a password reset page");
            render_server_error("Failed to render the password reset page.")
        }
    }
}

/// GET /user/password-reset: the form asking for an email address.
async fn request_form(State(state): State<AppState>, session: Session) -> Response {
    let mut context = tera::Context::new();
    context.insert("stage", "form");
    context.insert("csrf_token", &generate_csrf_token(&session).await);
    render_page(
        &state,
        &session,
        "user/password_reset_request.html",
        StatusCode::OK,
        context,
        false,
    )
    .await
}

/// Request a password reset.
///
/// POST /user/password-reset
///
/// Answers [`GENERIC_ANSWER`] whatever the address, as JSON or as a page with
/// no form token on it, so the page is the same for every address.
async fn request_reset(
    State(state): State<AppState>,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    session: Session,
    request: Request,
) -> Response {
    if sends_json(request.headers()) {
        let Json(input) = match Json::<RequestResetInput>::from_request(request, &state).await {
            Ok(input) => input,
            Err(rejection) => return rejection.into_response(),
        };
        queue_reset_mail(&state, client_ip.0, input.email).await;
        return Json(JsonSuccess {
            success: true,
            message: GENERIC_ANSWER.to_string(),
        })
        .into_response();
    }

    let Form(form) = match Form::<RequestResetForm>::from_request(request, &state).await {
        Ok(form) => form,
        Err(rejection) => return rejection.into_response(),
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    queue_reset_mail(&state, client_ip.0, form.email).await;

    let mut context = tera::Context::new();
    context.insert("stage", "sent");
    context.insert("message", GENERIC_ANSWER);
    render_page(
        &state,
        &session,
        "user/password_reset_request.html",
        StatusCode::OK,
        context,
        false,
    )
    .await
}

/// Check the two limits and, under both, mail a reset link in the background.
///
/// Two limits, both answered with the same generic answer and no mail rather
/// than a 429, so a limit cannot be used to learn anything either:
///
/// - per client address, on the `recovery` bucket (5 per 15 minutes), so one
///   source cannot send many mails;
/// - per destination address, on the `reset_mail` bucket (3 an hour), keyed on
///   the lower-cased address, so many sources cannot flood one inbox.
///
/// Looking the address up, creating a token and sending the mail all happen in
/// a spawned task. The request itself does the same work for every address
/// (the two limit checks) and answers, so the response time does not depend on
/// whether the address has an account.
async fn queue_reset_mail(state: &AppState, client_id: String, email: String) {
    let address = email.trim().to_lowercase();

    if state
        .rate_limiter()
        .check("recovery", &format!("password-reset:{client_id}"))
        .await
        .is_err()
    {
        info!(client = %state.runtime().client_ip_storage.kept_or_empty(&client_id), "password reset request over the per-client limit; no mail sent");
        return;
    }
    if state
        .rate_limiter()
        .check("reset_mail", &address)
        .await
        .is_err()
    {
        info!(client = %state.runtime().client_ip_storage.kept_or_empty(&client_id), "password reset request over the per-address limit; no mail sent");
        return;
    }

    tokio::spawn(send_reset_mail(state.clone(), email, client_id));
}

/// Look `email` up and, if it belongs to an account, queue the reset mail with
/// the `password_reset` email template. Runs after the request has been
/// answered.
///
/// The token is minted by the mail worker immediately before the send (BG18),
/// so it is never stored in the clear and a message delayed by a mail outage
/// carries a fresh link. One message per account per ten minutes: the dedupe
/// key makes a double submitted or retried request send once.
async fn send_reset_mail(state: AppState, email: String, client_id: String) {
    match state.users().find_by_mail(&email).await {
        Ok(Some(user)) => {
            let Some(email_service) = state.email() else {
                // SMTP not configured — log the token for development
                match PasswordResetToken::create(state.db(), user.id).await {
                    Ok((_, plain_token)) => {
                        tracing::debug!(
                            user_id = %user.id,
                            token = %plain_token,
                            "Password reset requested (SMTP not configured, token logged)"
                        );
                        tracing::debug!(
                            reset_url = format!("/user/password-reset/{}", plain_token),
                            "Reset URL for testing"
                        );
                    }
                    Err(e) => tracing::error!(error = %e, "failed to create reset token"),
                }
                return;
            };
            let site_name = crate::models::SiteConfig::site_name(state.db())
                .await
                .unwrap_or_else(|_| "Trovato".to_string());
            let reset_url = format!(
                "{}/user/password-reset/{}",
                email_service.site_url(),
                crate::services::email::SECRET_MARKER
            );
            let subject = format!("Password reset for {site_name}");
            let mut context = tera::Context::new();
            context.insert("site_name", &site_name);
            context.insert("action_url", &reset_url);
            context.insert("subject", &subject);
            let (html, text) = match crate::services::email_templates::render(
                state.theme().tera(),
                "password_reset",
                &context,
            ) {
                Ok(parts) => parts,
                Err(e) => {
                    tracing::error!(error = %e, "failed to render the password reset email");
                    return;
                }
            };
            let window = chrono::Utc::now().timestamp() / 600;
            let message = crate::services::email::NewMail::account(
                "password_reset",
                &user.mail,
                &subject,
                &text,
            )
            .html(html)
            .secret(crate::services::email::MailSecret::PasswordReset { user_id: user.id })
            .dedupe(format!("password-reset:{}:{window}", user.id));
            match email_service.queue_now(message).await {
                Ok(Some(_)) => info!(user_id = %user.id, "password reset email queued"),
                Ok(None) => info!(
                    user_id = %user.id,
                    "password reset email already queued in this window; not queued again"
                ),
                Err(e) => tracing::error!(error = %e, "failed to queue the password reset email"),
            }
        }
        Ok(None) => {
            // User not found - log but don't reveal to client
            info!(client = %state.runtime().client_ip_storage.kept_or_empty(&client_id), "password reset requested for non-existent email");
        }
        Err(e) => {
            tracing::error!(error = %e, "database error during password reset request");
        }
    }
}

/// Render the set-password page: the form (with `error` if any), or the page
/// saying the link is invalid or has expired.
async fn render_set_page(
    state: &AppState,
    session: &Session,
    token: &str,
    valid: bool,
    error: Option<&str>,
) -> Response {
    let mut context = tera::Context::new();
    let status = if valid {
        context.insert("stage", "form");
        context.insert("action", &format!("/user/password-reset/{token}"));
        context.insert("csrf_token", &generate_csrf_token(session).await);
        if let Some(error) = error {
            context.insert("error", error);
        }
        StatusCode::OK
    } else {
        context.insert("stage", "invalid");
        StatusCode::BAD_REQUEST
    };
    render_page(
        state,
        session,
        "user/password_reset_set.html",
        status,
        context,
        true,
    )
    .await
}

/// GET /user/password-reset/{token}: the set-password form, or a page saying
/// the link is invalid or has expired. Changes nothing.
async fn validate_token(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    let valid = match PasswordResetToken::find_valid(state.db(), &token).await {
        Ok(found) => found.is_some(),
        Err(e) => return AppError::internal_ctx(e, "validate reset token").into_response(),
    };

    if wants_json(&headers) {
        return if valid {
            Json(JsonSuccess {
                success: true,
                message: "Token is valid. You may set a new password.".to_string(),
            })
            .into_response()
        } else {
            AppError::bad_request("Invalid or expired reset token").into_response()
        };
    }

    render_set_page(&state, &session, &token, valid, None).await
}

/// Set new password using reset token.
///
/// POST /user/password-reset/{token}
async fn set_password(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    if sends_json(request.headers()) {
        let Json(input) = match Json::<SetPasswordInput>::from_request(request, &state).await {
            Ok(input) => input,
            Err(rejection) => return rejection.into_response(),
        };
        if let Err(msg) = validate_password(&input.password) {
            return AppError::bad_request(msg.to_string()).into_response();
        }
        return match reset_password(&state, &token, &input.password).await {
            Ok(true) => Json(JsonSuccess {
                success: true,
                message: "Password has been reset. You may now log in.".to_string(),
            })
            .into_response(),
            Ok(false) => AppError::bad_request("Invalid or expired reset token").into_response(),
            Err(e) => e.into_response(),
        };
    }

    let Form(form) = match Form::<SetPasswordForm>::from_request(request, &state).await {
        Ok(form) => form,
        Err(rejection) => return rejection.into_response(),
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    // Checked before the token is spent, so a typo keeps the link usable.
    if form.password != form.confirm_password {
        return render_set_page(
            &state,
            &session,
            &token,
            true,
            Some("Passwords do not match."),
        )
        .await;
    }
    if let Err(msg) = validate_password(&form.password) {
        return render_set_page(&state, &session, &token, true, Some(msg)).await;
    }

    match reset_password(&state, &token, &form.password).await {
        Ok(true) => {
            if let Err(e) = session
                .insert(SESSION_LOGIN_NOTICE, RESET_DONE_NOTICE)
                .await
            {
                tracing::warn!(error = %e, "failed to store the password reset notice");
            }
            Redirect::to("/user/login").into_response()
        }
        Ok(false) => render_set_page(&state, &session, &token, false, None).await,
        Err(e) => e.into_response(),
    }
}

/// Spend `token` and set the password of the account it belongs to.
///
/// Returns `false`, changing nothing, when the token does not exist, has
/// expired or was already spent. The token is spent first, in one statement,
/// so two concurrent requests with one token change the password at most
/// once.
async fn reset_password(state: &AppState, token: &str, password: &str) -> Result<bool, AppError> {
    let Some(user_id) = PasswordResetToken::consume(state.db(), token)
        .await
        .map_err(|e| AppError::internal_ctx(e, "consume reset token"))?
    else {
        return Ok(false);
    };

    // Update the password (anonymous context — user is not logged in)
    let anon = UserContext::anonymous();
    state
        .users()
        .update_password(user_id, password, &anon, "password_reset")
        .await
        .map_err(|e| AppError::internal_ctx(e, "update password"))?;

    // Whoever holds a session for this account now holds it under a password
    // that has just been replaced, possibly by the very person the reset is
    // locking out. End all of them; the user signs in afresh.
    crate::services::session_registry::end_user_sessions(
        state,
        user_id,
        None,
        None,
        "password_reset",
    )
    .await;

    // Invalidate any other tokens for this user
    if let Err(e) = PasswordResetToken::invalidate_user_tokens(state.db(), user_id).await {
        tracing::warn!(error = %e, user_id = %user_id, "failed to invalidate password reset tokens");
    }

    info!(user_id = %user_id, "password reset completed");
    Ok(true)
}

/// Create the password reset router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/user/password-reset",
            get(request_form).post(request_reset),
        )
        .route(
            "/user/password-reset/{token}",
            get(validate_token).post(set_password),
        )
}
