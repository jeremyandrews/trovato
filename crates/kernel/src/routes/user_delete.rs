//! Self-service account deletion.
//!
//! An administrator could delete a user; the user could not delete themselves. For
//! a site operated from the EU with open registration that is GDPR article 17, and
//! the ordinary reading is the same: Drupal 6 had cancel-own-account in core, and an
//! account you cannot close is not really yours.
//!
//! # The flow, and why it has three steps
//!
//! 1. `GET /user/delete` — re-authenticate. A session cookie is not enough
//!    authority to destroy an account: a borrowed laptop is the whole threat.
//! 2. `GET /user/delete/confirm` — say exactly what will happen, in full, before
//!    anything happens.
//! 3. `POST /user/delete/confirm` — do it.
//!
//! Step 2 exists separately from step 1 because "prove it is you" and "here is what
//! you are about to lose" are different questions, and answering them on one screen
//! means whoever is rushing reads neither.
//!
//! # Re-authentication
//!
//! There was no re-authentication machinery anywhere in the kernel to reuse: no
//! admin flow has one, and the WebAuthn routes authenticate *from scratch* into a
//! session rather than stepping an existing one up. So this adds a step-up scoped
//! to deletion:
//!
//! - **Password accounts** post their password to `POST /user/delete/reauth`.
//! - **Passwordless accounts** run a fresh WebAuthn assertion through
//!   `POST /user/delete/passkey/{start,finish}`.
//!
//! Both write [`SESSION_DELETE_REAUTH_AT`], and the confirm screens accept it for
//! [`REAUTH_WINDOW_SECS`]. The passkey ceremony is stored under its **own** session
//! key, never the login one: a login ceremony in flight must not be completable as
//! a deletion step-up, and vice versa.
//!
//! The passkey path needs JavaScript, because a WebAuthn ceremony does. The password
//! path is a plain form and does not.
//!
//! # What deletion means
//!
//! Stated here because it is policy and not implementation detail, and stated on the
//! confirmation screen in the same terms:
//!
//! - The account row is **deleted**.
//! - Authored items and comments are **reattributed to the anonymous author**, not
//!   destroyed. Content integrity wins: a thread with holes in it damages every
//!   other participant's record of a conversation they took part in. See
//!   [`crate::models::User::reattribute_content`], which is also where the
//!   foreign-key bug this uncovered is described.
//! - Sessions are **revoked everywhere**, through the Redis session registry.
//! - WebAuthn credentials and API tokens are **deleted** (both cascade).
//! - A security-audit event is emitted with the hashed account id, per the audit
//!   module's hashing rule.
//! - A confirmation email goes to the address on file, before the row goes, because
//!   afterwards there is no address to send to.
//! - `tap_user_delete` fires before the row is removed, so a plugin can clean up its
//!   own user-keyed rows while the user still exists.
//!
//! # The last administrator cannot leave
//!
//! A site with no active administrator cannot be administered back into having one.
//! The refusal is explicit, and audited.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use tower_sessions::Session;
use tracing::{info, warn};

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::form::csrf::generate_csrf_token;
use crate::models::user::ANONYMOUS_USER_ID;
use crate::services::webauthn::{PendingAuthentication, ceremony_expiry, ceremony_is_live};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, render_server_error, require_csrf, require_csrf_header, require_login,
};

/// Session key holding when the deletion step-up completed.
pub const SESSION_DELETE_REAUTH_AT: &str = "delete_reauth_at";

/// Session key holding the deletion-scoped WebAuthn ceremony.
///
/// Deliberately not `SESSION_WEBAUTHN_AUTH`: a login ceremony must not be
/// completable as a deletion step-up, nor the reverse.
pub const SESSION_DELETE_CEREMONY: &str = "delete_webauthn_auth";

/// How long a step-up stays good. Long enough to read the confirmation screen,
/// short enough that a walk-away does not leave the account deletable.
pub const REAUTH_WINDOW_SECS: i64 = 300;

/// Whether a deletion must be refused to keep the site administrable.
///
/// A site with no active administrator cannot be administered back into having one,
/// so the last one cannot leave. Pulled out as a pure function because the condition
/// is the whole guard, and it is not otherwise testable without controlling a
/// database's entire administrator population.
pub fn blocks_last_admin(is_admin: bool, active_admins: i64) -> bool {
    is_admin && active_admins <= 1
}

/// Whether a step-up recorded at `at` is still good at `now`.
///
/// A timestamp in the future is refused rather than trusted: a clock that moved
/// backwards should not hand out an indefinite window.
pub fn reauth_is_fresh(at: i64, now: i64) -> bool {
    at <= now && now - at <= REAUTH_WINDOW_SECS
}

#[derive(Debug, Deserialize)]
struct ReauthForm {
    #[serde(rename = "_token")]
    token: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct PasskeyFinishRequest {
    credential: Box<webauthn_rs::prelude::PublicKeyCredential>,
}

/// Read the step-up marker from the session, if it is fresh.
async fn step_up_is_fresh(session: &Session) -> bool {
    let at: Option<i64> = session.get(SESSION_DELETE_REAUTH_AT).await.ok().flatten();
    at.is_some_and(|at| reauth_is_fresh(at, chrono::Utc::now().timestamp()))
}

async fn mark_step_up(session: &Session) {
    let _ = session
        .insert(SESSION_DELETE_REAUTH_AT, chrono::Utc::now().timestamp())
        .await;
}

/// `GET /user/delete` — the re-authentication screen.
async fn delete_start(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // A step-up already in hand skips straight to the consequences.
    if step_up_is_fresh(&session).await {
        return Redirect::to("/user/delete/confirm").into_response();
    }

    render_step_up(&state, &session, &user, None).await
}

/// Render the step-up screen, offering whichever methods the account actually has.
///
/// Both, when it has both. An account with a password and a passkey should not be
/// made to type a password it may not remember when the authenticator is right
/// there, and the reverse when the authenticator is at home.
async fn render_step_up(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    error: Option<&str>,
) -> Response {
    let has_passkey = crate::models::WebauthnCredential::list_for_user(state.db(), user.id)
        .await
        .map(|credentials| !credentials.is_empty())
        .unwrap_or(false);

    let csrf_token = generate_csrf_token(session).await;
    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("has_password", &!user.pass.is_empty());
    context.insert("has_passkey", &has_passkey);
    context.insert("error", &error);
    render_user_template(state, session, "user/delete.html", context).await
}

/// `POST /user/delete/reauth` — verify a password.
async fn delete_reauth(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<ReauthForm>,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Same bucket as a password change: this is a password guess against a known
    // account, and the limiter should not care which form it arrived through.
    let ip = client_ip(&headers);
    if let Err(retry_after) = state.rate_limiter().check("password", &ip).await {
        return too_many(retry_after);
    }

    if user.pass.is_empty() {
        // A passwordless account has no password to check, and saying "wrong
        // password" would be a lie.
        return reauth_failed(
            &state,
            &session,
            &user,
            "This account has no password. Use your passkey.",
        )
        .await;
    }

    if !user.verify_password(&form.password) {
        warn!(user_id = %user.id, "account deletion step-up failed");
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::LoginFailed)
                    .user(user.id)
                    .ip(ip)
                    .detail("method", "password")
                    .detail("purpose", "account_deletion_step_up"),
            )
            .await;
        return reauth_failed(&state, &session, &user, "That password is not right.").await;
    }

    mark_step_up(&session).await;
    Redirect::to("/user/delete/confirm").into_response()
}

/// Re-render the step-up screen with a message.
async fn reauth_failed(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    message: &str,
) -> Response {
    render_step_up(state, session, user, Some(message)).await
}

/// `POST /user/delete/passkey/start` — begin a deletion-scoped assertion.
async fn passkey_start(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
) -> Response {
    let Ok(user) = require_login(&state, &session).await else {
        return json_error(StatusCode::UNAUTHORIZED, "Not signed in.");
    };
    if require_csrf_header(&session, &headers).await.is_err() {
        return json_error(StatusCode::FORBIDDEN, "Invalid or missing CSRF token.");
    }

    let credentials =
        match crate::models::WebauthnCredential::list_for_user(state.db(), user.id).await {
            Ok(credentials) => credentials,
            Err(e) => {
                tracing::error!(error = %e, "failed to load credentials for a deletion step-up");
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not start passkey verification.",
                );
            }
        };
    let passkeys: Vec<_> = credentials
        .iter()
        .filter_map(|c| c.passkey().ok())
        .collect();
    if passkeys.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "This account has no passkey.");
    }

    let (challenge, auth_state) = match state.webauthn().start_passkey_authentication(&passkeys) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, user_id = %user.id, "failed to start a deletion step-up");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start passkey verification.",
            );
        }
    };

    let pending = PendingAuthentication {
        state: auth_state,
        expires_at: ceremony_expiry(chrono::Utc::now().timestamp()),
        user_id: user.id,
    };
    if session
        .insert(SESSION_DELETE_CEREMONY, pending)
        .await
        .is_err()
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not start passkey verification.",
        );
    }

    Json(challenge).into_response()
}

/// `POST /user/delete/passkey/finish` — verify the assertion.
async fn passkey_finish(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<PasskeyFinishRequest>,
) -> Response {
    let Ok(user) = require_login(&state, &session).await else {
        return json_error(StatusCode::UNAUTHORIZED, "Not signed in.");
    };
    if require_csrf_header(&session, &headers).await.is_err() {
        return json_error(StatusCode::FORBIDDEN, "Invalid or missing CSRF token.");
    }

    // Single use: taken before verifying, so a replay has nothing to verify.
    let pending: Option<PendingAuthentication> =
        session.remove(SESSION_DELETE_CEREMONY).await.ok().flatten();
    let Some(pending) = pending else {
        return json_error(StatusCode::BAD_REQUEST, "No verification in progress.");
    };

    // The ceremony belongs to the session's account, always. Without this a
    // ceremony started as one account could be finished as another.
    if pending.user_id != user.id {
        warn!(
            user_id = %user.id,
            ceremony_user = %pending.user_id,
            "deletion step-up ceremony belongs to a different account"
        );
        return json_error(StatusCode::BAD_REQUEST, "No verification in progress.");
    }

    if !ceremony_is_live(pending.expires_at, chrono::Utc::now().timestamp()) {
        return json_error(StatusCode::BAD_REQUEST, "Verification expired. Try again.");
    }

    match state
        .webauthn()
        .finish_passkey_authentication(&body.credential, &pending.state)
    {
        Ok(_) => {
            mark_step_up(&session).await;
            Json(serde_json::json!({ "verified": true })).into_response()
        }
        Err(e) => {
            warn!(error = %e, user_id = %user.id, "deletion step-up assertion failed");
            state
                .security_audit()
                .emit(
                    SecurityEvent::failure(SecurityEventKind::LoginFailed)
                        .user(user.id)
                        .detail("method", "passkey")
                        .detail("purpose", "account_deletion_step_up"),
                )
                .await;
            json_error(StatusCode::UNAUTHORIZED, "That passkey was not accepted.")
        }
    }
}

/// `GET /user/delete/confirm` — exactly what will happen.
async fn delete_confirm_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    if !step_up_is_fresh(&session).await {
        return Redirect::to("/user/delete").into_response();
    }

    let items = crate::models::Item::count_by_author(state.db(), user.id)
        .await
        .unwrap_or(0);
    let comments = crate::models::Comment::count_by_author(state.db(), user.id)
        .await
        .unwrap_or(0);
    let last_admin = state
        .users()
        .active_admin_count()
        .await
        .map(|count| blocks_last_admin(user.is_admin, count))
        .unwrap_or(false);

    let csrf_token = generate_csrf_token(&session).await;
    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("username", &user.name);
    context.insert("email", &user.mail);
    context.insert("items", &items);
    context.insert("comments", &comments);
    context.insert("last_admin", &last_admin);
    render_user_template(&state, &session, "user/delete-confirm.html", context).await
}

/// `POST /user/delete/confirm` — delete the account.
async fn delete_confirm_submit(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    if !step_up_is_fresh(&session).await {
        return Redirect::to("/user/delete").into_response();
    }
    // The anonymous account is a sentinel and cannot be signed in as, but the
    // guard is cheap and the consequence of being wrong is a site with no
    // anonymous author.
    if user.id == ANONYMOUS_USER_ID {
        return render_server_error("That account cannot be deleted.");
    }

    let ip = client_ip(&headers);

    // The last administrator cannot leave: a site with no active administrator
    // cannot be administered back into having one.
    if user.is_admin {
        match state.users().active_admin_count().await {
            Ok(count) if blocks_last_admin(user.is_admin, count) => {
                state
                    .security_audit()
                    .emit(
                        SecurityEvent::failure(SecurityEventKind::AccountDeletionBlocked)
                            .user(user.id)
                            .ip(ip)
                            .detail("reason", "last_active_administrator"),
                    )
                    .await;
                return (
                    StatusCode::CONFLICT,
                    axum::response::Html(
                        "<h1>Account not deleted</h1><p>You are the only active \
                         administrator. Make somebody else an administrator first, or \
                         the site would be left with nobody who can administer it.</p>\
                         <p><a href=\"/user/profile\">Back to my account</a></p>"
                            .to_string(),
                    ),
                )
                    .into_response();
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to count administrators");
                return render_server_error("Could not verify the administrator count.");
            }
        }
    }

    let items = crate::models::Item::count_by_author(state.db(), user.id)
        .await
        .unwrap_or(0);
    let comments = crate::models::Comment::count_by_author(state.db(), user.id)
        .await
        .unwrap_or(0);

    // Before the row goes, because afterwards there is no address to send to. A
    // failure here does not stop the deletion: the person asked for the account to
    // go, and an unsendable courtesy note is not a reason to keep it.
    send_farewell(&state, &user).await;

    let acting = crate::tap::UserContext::authenticated(user.id, Vec::new());
    match state.users().delete(user.id, &acting).await {
        Ok(true) => {}
        Ok(false) => return render_server_error("That account no longer exists."),
        Err(e) => {
            tracing::error!(error = %e, user_id = %user.id, "self-service account deletion failed");
            return render_server_error("Could not delete the account.");
        }
    }

    // Every session, everywhere, including this one. `None` because there is no
    // session worth keeping for an account that no longer exists.
    let revoked = state
        .session_registry()
        .revoke_all_except(user.id, None)
        .await
        .map(|entries| entries.len())
        .unwrap_or(0);

    // Deliberately **no** `.user(...)`. Two reasons, and they point the same way:
    // `security_audit_log.user_id` references `users` with `ON DELETE SET NULL`, so
    // by the time this row is written the account it would name is gone and the
    // insert would fail on the foreign key; and an audit row carrying the raw id of
    // an account that was just erased would undercut the erasure. The hashed subject
    // is the record, which is what the audit module's hashing rule is for.
    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::AccountDeleted)
                .subject(&user.id.to_string())
                .ip(ip)
                .detail("by", "self")
                .detail("items_reattributed", items)
                .detail("comments_reattributed", comments)
                .detail("sessions_revoked", revoked),
        )
        .await;

    info!(
        user_id = %user.id,
        items_reattributed = items,
        comments_reattributed = comments,
        sessions_revoked = revoked,
        "account deleted by its holder"
    );

    // And this session, which the registry may not have indexed.
    let _ = session.flush().await;

    Redirect::to("/?account_deleted=1").into_response()
}

/// Tell the address on file that the account is gone.
async fn send_farewell(state: &AppState, user: &crate::models::User) {
    let Some(email) = state.email() else {
        return;
    };
    let site_name = crate::models::SiteConfig::site_name(state.db())
        .await
        .unwrap_or_else(|_| "Trovato".to_string());

    let mut context = tera::Context::new();
    context.insert("site_name", &site_name);
    context.insert("username", &user.name);

    let tera = state.theme().tera();
    let text = tera
        .render("email/account_deleted.txt", &context)
        .unwrap_or_else(|_| format!("Your account on {site_name} has been deleted, as you asked."));
    let html = tera.render("email/account_deleted.html", &context).ok();

    if let Err(e) = email
        .send_templated(
            &user.mail,
            &format!("Your {site_name} account has been deleted"),
            &text,
            html.as_deref(),
        )
        .await
    {
        // Logged and swallowed on purpose: see the call site.
        warn!(error = %e, "failed to send an account deletion confirmation");
    }
}

/// Render a template with the site context, as the other user pages do.
async fn render_user_template(
    state: &AppState,
    session: &Session,
    template: &str,
    mut context: tera::Context,
) -> Response {
    super::helpers::inject_site_context(state, session, &mut context, "/user/delete").await;
    match state.theme().tera().render(template, &context) {
        Ok(html) => axum::response::Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template = %template, "failed to render");
            render_server_error("Could not render the page.")
        }
    }
}

fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .unwrap_or("0.0.0.0")
        .to_string()
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn too_many(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, retry_after.to_string())],
        axum::response::Html(
            "<h1>Too many attempts</h1><p>Wait a moment and try again.</p>".to_string(),
        ),
    )
        .into_response()
}

/// Account-deletion routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/delete", get(delete_start))
        .route("/user/delete/reauth", post(delete_reauth))
        .route("/user/delete/passkey/start", post(passkey_start))
        .route("/user/delete/passkey/finish", post(passkey_finish))
        .route(
            "/user/delete/confirm",
            get(delete_confirm_form).post(delete_confirm_submit),
        )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_step_up_is_fresh_inside_the_window_and_not_outside_it() {
        let now = 1_000_000;
        assert!(reauth_is_fresh(now, now), "just completed");
        assert!(
            reauth_is_fresh(now - REAUTH_WINDOW_SECS, now),
            "at the edge"
        );
        assert!(
            !reauth_is_fresh(now - REAUTH_WINDOW_SECS - 1, now),
            "one second past the window"
        );
    }

    #[test]
    fn a_step_up_from_the_future_is_refused() {
        let now = 1_000_000;
        assert!(
            !reauth_is_fresh(now + 1, now),
            "a clock that moved backwards must not hand out an open-ended window"
        );
    }

    #[test]
    fn only_the_last_active_administrator_is_blocked() {
        assert!(
            blocks_last_admin(true, 1),
            "the only administrator cannot leave"
        );
        assert!(
            blocks_last_admin(true, 0),
            "a site that somehow has no active administrator must not lose another"
        );
        assert!(
            !blocks_last_admin(true, 2),
            "an administrator with a colleague may leave"
        );
        assert!(
            !blocks_last_admin(false, 1),
            "a non-administrator is never the last administrator, whatever the count"
        );
        assert!(!blocks_last_admin(false, 0));
    }

    #[test]
    fn the_ceremony_key_is_not_the_login_ceremony_key() {
        assert_ne!(
            SESSION_DELETE_CEREMONY,
            crate::services::webauthn::SESSION_WEBAUTHN_AUTH,
            "a login ceremony must not be completable as a deletion step-up"
        );
    }
}
