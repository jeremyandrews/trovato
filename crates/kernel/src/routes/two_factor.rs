//! The user-facing TOTP second-factor pages (B304): status, setup, confirm,
//! recovery codes, removal, and the post-password verify step a confirmed
//! enrolment sends a login through.
//!
//! Every page here requires a signed-in user (the status/setup/confirm/
//! recovery-codes/remove pages) except `/user/login/verify`, which exists
//! precisely because no session exists yet at that point.

use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::routes::auth::{
    PendingSecondFactor, SESSION_ENROLLMENT_REQUIRED, SESSION_PENDING_SECOND_FACTOR,
};
use crate::routes::helpers::{
    inject_site_context, render_server_error, require_csrf, require_login,
};
use crate::services::totp;
use crate::state::AppState;

/// Session key: the plain recovery codes a just-confirmed enrolment issued,
/// shown exactly once by `GET /user/two-factor/recovery-codes` and removed
/// the moment that page reads them.
const SESSION_RECOVERY_CODES_DISPLAY: &str = "totp_recovery_codes_display";

/// Session key: a one-shot error flash for the setup page, shown and cleared
/// on the next `GET /user/two-factor/setup`.
const SESSION_SETUP_ERROR: &str = "totp_setup_error";

// ─── Status page ──────────────────────────────────────────────────────────

/// GET /user/two-factor
async fn status_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let enrolled = crate::services::second_factor::has_confirmed_totp(&state, user.id).await;
    let recovery_codes_remaining = if enrolled {
        totp::remaining_recovery_codes(state.db(), user.id)
            .await
            .unwrap_or(0)
    } else {
        0
    };
    let requirement =
        crate::services::second_factor::account_subject_to_requirement(&state, &user).await;
    let has_passkey = crate::services::second_factor::has_passkey(&state, user.id).await;

    let mut context = tera::Context::new();
    context.insert("enrolled", &enrolled);
    context.insert("recovery_codes_remaining", &recovery_codes_remaining);
    context.insert("requires_second_factor", &requirement);
    context.insert("has_passkey", &has_passkey);
    context.insert("enrolment_available", &totp::enrolment_available(&state));
    context.insert(
        "csrf_token",
        &crate::form::csrf::generate_csrf_token(&session).await,
    );
    inject_site_context(&state, &session, &mut context, "/user/two-factor").await;

    render(&state, "user/two-factor.html", context)
}

// ─── Setup ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CsrfForm {
    #[serde(rename = "_token")]
    token: String,
}

/// POST /user/two-factor/setup — start (or restart) enrolment with a fresh
/// secret, then redirect to the page that shows it.
async fn setup_start(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CsrfForm>,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    if !totp::enrolment_available(&state) {
        return render_server_error(totp::UNAVAILABLE_MESSAGE);
    }

    let site_name = crate::models::SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    if let Err(e) = totp::start_enrolment(&state, user.id, &user.name, &site_name).await {
        tracing::error!(error = %e, user_id = %user.id, "failed to start TOTP enrolment");
        return render_server_error("Could not start setting up an authenticator.");
    }

    Redirect::to("/user/two-factor/setup").into_response()
}

/// GET /user/two-factor/setup — the QR code and manual secret for the
/// pending (unconfirmed) enrolment this account started.
async fn setup_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if !totp::enrolment_available(&state) {
        return render_server_error(totp::UNAVAILABLE_MESSAGE);
    }

    let Some(row) = crate::models::user_totp::UserTotp::find(state.db(), user.id)
        .await
        .ok()
        .flatten()
        .filter(|r| r.confirmed_at.is_none())
    else {
        return Redirect::to("/user/two-factor").into_response();
    };
    let Some(secret_box) = state.secret_box() else {
        return render_server_error(totp::UNAVAILABLE_MESSAGE);
    };
    let secret_bytes = match secret_box.open(
        crate::secret_box::PURPOSE_TOTP_SECRET,
        user.id.as_bytes(),
        &row.secret,
    ) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!(error = %e, user_id = %user.id, "failed to open a pending TOTP secret");
            return render_server_error("Could not display the authenticator setup.");
        }
    };
    let site_name = crate::models::SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let rendered = match totp::render_pending_setup(&secret_bytes, &user.name, &site_name) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to render the pending TOTP setup");
            return render_server_error("Could not display the authenticator setup.");
        }
    };

    let error: Option<String> = session.get(SESSION_SETUP_ERROR).await.ok().flatten();
    if error.is_some() {
        let _ = session.remove::<String>(SESSION_SETUP_ERROR).await;
    }

    let mut context = tera::Context::new();
    context.insert("manual_secret", &rendered.manual_secret);
    context.insert("qr_svg", &rendered.qr_svg);
    if let Some(error) = &error {
        context.insert("error", error);
    }
    context.insert(
        "csrf_token",
        &crate::form::csrf::generate_csrf_token(&session).await,
    );
    inject_site_context(&state, &session, &mut context, "/user/two-factor/setup").await;

    render(&state, "user/two-factor-setup.html", context)
}

#[derive(Deserialize)]
struct ConfirmForm {
    #[serde(rename = "_token")]
    token: String,
    code: String,
}

/// POST /user/two-factor/confirm — check the code; on success, confirm the
/// enrolment, issue recovery codes, and show them once.
async fn confirm_submit(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Form(form): Form<ConfirmForm>,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // The same per-account, per-address throttle `password_change` applies to
    // a wrong current password: `LockoutService`, keyed on the account's name
    // and the caller's address.
    if let Ok(true) = state.lockout().is_locked(&user.name, &client_ip.0).await {
        return setup_error_redirect(&session, "Too many attempts. Try again later.").await;
    }
    if let Err(retry_after) = state
        .rate_limiter()
        .check("password", &user.id.to_string())
        .await
    {
        return crate::middleware::rate_limit_response(retry_after);
    }

    match totp::confirm_enrolment(&state, user.id, &form.code).await {
        Ok(Some(codes)) => {
            if let Err(e) = state
                .lockout()
                .clear_attempts(&user.name, &client_ip.0)
                .await
            {
                tracing::warn!(error = %e, "failed to clear lockout attempts after TOTP confirm");
            }
            if let Err(e) = session.insert(SESSION_RECOVERY_CODES_DISPLAY, codes).await {
                tracing::warn!(error = %e, "failed to stash recovery codes for one-time display");
            }
            // Enrolling satisfies the requirement at once.
            let _ = session.remove::<bool>(SESSION_ENROLLMENT_REQUIRED).await;
            state
                .security_audit()
                .emit(SecurityEvent::new(SecurityEventKind::TotpEnrolled).user(user.id))
                .await;
            Redirect::to("/user/two-factor/recovery-codes").into_response()
        }
        Ok(None) => {
            if let Err(e) = state
                .lockout()
                .record_failed_attempt(&user.name, &client_ip.0)
                .await
            {
                tracing::warn!(error = %e, "failed to record a failed TOTP confirm attempt");
            }
            setup_error_redirect(&session, "That code did not verify. Try again.").await
        }
        Err(e) => {
            tracing::error!(error = %e, user_id = %user.id, "failed to confirm TOTP enrolment");
            render_server_error("Could not confirm the authenticator.")
        }
    }
}

/// Stash a one-shot error flash and redirect back to the setup page, which
/// re-derives the QR code and secret from the still-pending row.
async fn setup_error_redirect(session: &Session, error: &str) -> Response {
    if let Err(e) = session.insert(SESSION_SETUP_ERROR, error).await {
        tracing::warn!(error = %e, "failed to stash the setup-page error flash");
    }
    Redirect::to("/user/two-factor/setup").into_response()
}

/// GET /user/two-factor/recovery-codes — the one-time display.
async fn recovery_codes_page(State(state): State<AppState>, session: Session) -> Response {
    let _user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let codes: Option<Vec<String>> = session
        .get(SESSION_RECOVERY_CODES_DISPLAY)
        .await
        .ok()
        .flatten();
    let Some(codes) = codes else {
        return Redirect::to("/user/two-factor").into_response();
    };
    let _ = session
        .remove::<Vec<String>>(SESSION_RECOVERY_CODES_DISPLAY)
        .await;

    let mut context = tera::Context::new();
    context.insert("recovery_codes", &codes);
    inject_site_context(
        &state,
        &session,
        &mut context,
        "/user/two-factor/recovery-codes",
    )
    .await;
    render(&state, "user/two-factor-recovery-codes.html", context)
}

// ─── Remove ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RemoveForm {
    #[serde(rename = "_token")]
    token: String,
    code: String,
}

/// POST /user/two-factor/remove — needs a current code; refused when a role
/// requires a second factor and the account has no passkey.
async fn remove_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RemoveForm>,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    if crate::services::second_factor::account_subject_to_requirement(&state, &user).await
        && !crate::services::second_factor::has_passkey(&state, user.id).await
    {
        return crate::routes::helpers::render_forbidden(
            "This account's role requires a second factor; register a passkey before removing \
             the authenticator.",
        );
    }

    match totp::verify_code(&state, user.id, &form.code).await {
        Ok(true) => {
            if let Err(e) = totp::remove_enrolment(state.db(), user.id).await {
                tracing::error!(error = %e, user_id = %user.id, "failed to remove TOTP enrolment");
                return render_server_error("Could not remove the authenticator.");
            }
            state
                .security_audit()
                .emit(SecurityEvent::new(SecurityEventKind::TotpRemoved).user(user.id))
                .await;
            Redirect::to("/user/two-factor").into_response()
        }
        _ => {
            let mut context = tera::Context::new();
            context.insert("error", "That code did not verify. Try again.");
            context.insert("enrolled", &true);
            context.insert(
                "recovery_codes_remaining",
                &totp::remaining_recovery_codes(state.db(), user.id)
                    .await
                    .unwrap_or(0),
            );
            context.insert(
                "requires_second_factor",
                &crate::services::second_factor::account_subject_to_requirement(&state, &user)
                    .await,
            );
            context.insert(
                "has_passkey",
                &crate::services::second_factor::has_passkey(&state, user.id).await,
            );
            context.insert("enrolment_available", &totp::enrolment_available(&state));
            context.insert(
                "csrf_token",
                &crate::form::csrf::generate_csrf_token(&session).await,
            );
            inject_site_context(&state, &session, &mut context, "/user/two-factor").await;
            render(&state, "user/two-factor.html", context)
        }
    }
}

// ─── Login verify step ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VerifyForm {
    #[serde(rename = "_token")]
    token: String,
    code: String,
}

/// GET /user/login/verify
async fn verify_page(State(state): State<AppState>, session: Session) -> Response {
    let Some(pending): Option<PendingSecondFactor> = session
        .get(SESSION_PENDING_SECOND_FACTOR)
        .await
        .ok()
        .flatten()
    else {
        return Redirect::to("/user/login").into_response();
    };
    if !pending.is_live_now() {
        let _ = session
            .remove::<PendingSecondFactor>(SESSION_PENDING_SECOND_FACTOR)
            .await;
        return Redirect::to("/user/login").into_response();
    }

    let mut context = tera::Context::new();
    context.insert(
        "csrf_token",
        &crate::form::csrf::generate_csrf_token(&session).await,
    );
    inject_site_context(&state, &session, &mut context, "/user/login/verify").await;
    render(&state, "user/login-verify.html", context)
}

/// POST /user/login/verify
async fn verify_submit(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: axum::http::HeaderMap,
    Form(form): Form<VerifyForm>,
) -> Response {
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    let Some(pending): Option<PendingSecondFactor> = session
        .get(SESSION_PENDING_SECOND_FACTOR)
        .await
        .ok()
        .flatten()
    else {
        return Redirect::to("/user/login").into_response();
    };
    if !pending.is_live_now() {
        let _ = session
            .remove::<PendingSecondFactor>(SESSION_PENDING_SECOND_FACTOR)
            .await;
        return Redirect::to("/user/login").into_response();
    }

    let Some(user) = state
        .users()
        .find_by_id(pending.user_id)
        .await
        .ok()
        .flatten()
        .filter(|u| u.is_active())
    else {
        let _ = session
            .remove::<PendingSecondFactor>(SESSION_PENDING_SECOND_FACTOR)
            .await;
        return Redirect::to("/user/login").into_response();
    };

    let method = if totp::verify_code(&state, user.id, &form.code)
        .await
        .unwrap_or(false)
    {
        Some("password+totp")
    } else if totp::verify_recovery_code(&state, user.id, &form.code)
        .await
        .unwrap_or(false)
    {
        state
            .security_audit()
            .emit(SecurityEvent::new(SecurityEventKind::TotpRecoveryCodeUsed).user(user.id))
            .await;
        Some("password+recovery_code")
    } else {
        None
    };

    let Some(method) = method else {
        if let Err(e) = state
            .lockout()
            .record_failed_attempt(&user.name, &client_ip.0)
            .await
        {
            tracing::warn!(error = %e, "failed to record a failed verify-step attempt");
        }
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::LoginFailed)
                    .user(user.id)
                    .ip(client_ip.0.clone())
                    .detail("method", "password")
                    .detail("reason", "second_factor"),
            )
            .await;
        let mut context = tera::Context::new();
        context.insert("error", "That code did not verify. Try again.");
        context.insert(
            "csrf_token",
            &crate::form::csrf::generate_csrf_token(&session).await,
        );
        inject_site_context(&state, &session, &mut context, "/user/login/verify").await;
        return render(&state, "user/login-verify.html", context);
    };

    if let Err(e) = state
        .lockout()
        .clear_attempts(&user.name, &client_ip.0)
        .await
    {
        tracing::warn!(error = %e, "failed to clear lockout attempts after verify-step success");
    }
    if let Err(e) = state.users().record_login(&user).await {
        tracing::warn!(error = %e, user_id = %user.id, "failed to record login");
    }
    let destination = pending
        .destination
        .as_deref()
        .filter(|d| crate::routes::auth::is_safe_destination(d));
    let _ = session
        .remove::<PendingSecondFactor>(SESSION_PENDING_SECOND_FACTOR)
        .await;
    if let Err(e) =
        crate::routes::auth::setup_session(&state, &session, user.id, pending.remember_me).await
    {
        tracing::error!(error = %e, "failed to establish a session after the second-factor verify step");
        return render_server_error("Sign-in failed. Try again.");
    }
    crate::routes::auth::apply_password_age_flag(&state, &session, &user).await;

    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut event = SecurityEvent::new(SecurityEventKind::LoginSucceeded)
        .user(user.id)
        .ip(client_ip.0)
        .detail("method", method);
    if !ua.is_empty() {
        event = event.user_agent(ua);
    }
    state.security_audit().emit(event).await;

    Redirect::to(destination.unwrap_or("/")).into_response()
}

// ─── Shared render helper ───────────────────────────────────────────────────

fn render(state: &AppState, template: &str, context: tera::Context) -> Response {
    match state.theme().tera().render(template, &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template, "failed to render template");
            render_server_error("This page could not be displayed.")
        }
    }
}

/// Build the two-factor and login-verify router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/two-factor", get(status_page))
        .route("/user/two-factor/setup", get(setup_page).post(setup_start))
        .route("/user/two-factor/confirm", post(confirm_submit))
        .route("/user/two-factor/recovery-codes", get(recovery_codes_page))
        .route("/user/two-factor/remove", post(remove_submit))
        .route("/user/login/verify", get(verify_page).post(verify_submit))
}
