//! WebAuthn/passkey ceremony endpoints (FR-7a, design §1).
//!
//! Both ceremonies are two round trips of **JSON** — `/start` emits the options
//! the browser hands to `navigator.credentials.create()` / `.get()`, `/finish`
//! verifies what comes back. Exposing them as JSON endpoints (rather than an SSR
//! variant plus an API variant) means one ceremony implementation with two entry
//! pages, matching the FR-30 "one retrieval path, two render tails" posture: the
//! SSR pages ship the glue JS and call exactly these endpoints, and a pure-API
//! client drives them directly.
//!
//! The in-flight ceremony state lives in the Redis-backed **session** (design
//! §1, D-34) under [`SESSION_WEBAUTHN_REG`] / [`SESSION_WEBAUTHN_AUTH`], which
//! binds each challenge to the browser that requested it and needs no new store.
//!
//! On successful authentication `/login/finish` establishes the session through
//! the existing `routes::auth::setup_session`, so `session.cycle_id()` fires
//! after the auth-state change with no new code on that seam (AC-2).

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use serde::Deserialize;
use tower_sessions::Session;
use tracing::{info, warn};
use uuid::Uuid;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::models::WebauthnCredential;
use crate::models::webauthn_credential::{counter_regressed, encode_credential_id};
use crate::routes::helpers::{require_csrf_header, require_login};
use crate::services::webauthn::{
    PendingAuthentication, PendingRegistration, SESSION_WEBAUTHN_AUTH, SESSION_WEBAUTHN_REG,
    ceremony_expiry, ceremony_is_live,
};
use crate::state::AppState;

/// The generic failure a ceremony endpoint returns.
///
/// Ceremony errors are deliberately terse and uniform: a caller learns that the
/// ceremony failed, not which of attestation, challenge expiry, origin binding,
/// or credential lookup failed. The specific cause goes to the log and the audit
/// stream, where it is useful, rather than to an unauthenticated caller, where
/// it is an oracle.
fn ceremony_error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The client IP and User-Agent for an audit event.
fn request_context(
    headers: &HeaderMap,
    client_ip: &crate::middleware::ClientIp,
) -> (String, String) {
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (client_ip.0.clone(), ua)
}

// ─── Registration (Story 4.1) ────────────────────────────────────────────────

/// Body of `POST /user/webauthn/register/finish`.
#[derive(Deserialize)]
pub struct RegisterFinishRequest {
    /// What `navigator.credentials.create()` returned, serialized.
    pub credential: RegisterPublicKeyCredential,
    /// Optional user-assigned label for the new passkey.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// `POST /user/webauthn/register/start` — begin registering a passkey.
///
/// Requires an authenticated session: registering a passkey is an operation on
/// an existing account, never a way to create one. Already-registered
/// credentials are passed as `exclude_credentials` so an authenticator that is
/// already enrolled declines rather than silently creating a duplicate.
async fn register_start(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }

    let existing = match WebauthnCredential::list_for_user(state.db(), user.id).await {
        Ok(creds) => creds,
        Err(e) => {
            tracing::error!(error = %e, user_id = %user.id, "failed to load existing credentials");
            return ceremony_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start registration.",
            );
        }
    };

    let exclude: Vec<_> = existing
        .iter()
        .filter_map(|c| c.passkey().ok().map(|p| p.cred_id().clone()))
        .collect();

    let (challenge, reg_state) = match state.webauthn().start_passkey_registration(
        user.id,
        &user.name,
        &user.name,
        if exclude.is_empty() {
            None
        } else {
            Some(exclude)
        },
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, user_id = %user.id, "failed to start passkey registration");
            return ceremony_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start registration.",
            );
        }
    };

    let pending = PendingRegistration {
        state: reg_state,
        expires_at: ceremony_expiry(chrono::Utc::now().timestamp()),
        user_id: user.id,
    };
    if let Err(e) = session.insert(SESSION_WEBAUTHN_REG, pending).await {
        tracing::error!(error = %e, "failed to store registration ceremony state");
        return ceremony_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not start registration.",
        );
    }

    // CSRF tokens are single-use (`form::csrf::verify_csrf_token` consumes the
    // one it matched), and this ceremony is two round trips. Rather than drop
    // CSRF from the second leg, `/start` mints the token `/finish` will need and
    // hands it back with the challenge. Both legs stay protected.
    let next_token = crate::form::csrf::generate_csrf_token(&session).await;

    Json(serde_json::json!({
        "options": challenge,
        "csrf_token": next_token,
    }))
    .into_response()
}

/// `POST /user/webauthn/register/finish` — verify and store the new passkey.
async fn register_finish(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: HeaderMap,
    Json(body): Json<RegisterFinishRequest>,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }
    let (ip, ua) = request_context(&headers, &client_ip);

    let audit_failure = |reason: &'static str| {
        SecurityEvent::failure(SecurityEventKind::PasskeyRegistrationFailed)
            .user(user.id)
            .ip(ip.clone())
            .user_agent(ua.clone())
            .detail("reason", reason)
    };

    // The ceremony is single-use: take it out of the session before verifying,
    // so a failed or replayed `/finish` cannot be retried against the same
    // challenge.
    let pending: Option<PendingRegistration> =
        session.remove(SESSION_WEBAUTHN_REG).await.ok().flatten();
    let Some(pending) = pending else {
        state
            .security_audit()
            .emit(audit_failure("no_ceremony_in_progress"))
            .await;
        return ceremony_error(StatusCode::BAD_REQUEST, "No registration in progress.");
    };

    if !ceremony_is_live(pending.expires_at, chrono::Utc::now().timestamp()) {
        state
            .security_audit()
            .emit(audit_failure("ceremony_expired"))
            .await;
        return ceremony_error(StatusCode::BAD_REQUEST, "Registration expired. Try again.");
    }

    // A ceremony started as one account must never be completed as another —
    // the session could have been re-authenticated in between.
    if pending.user_id != user.id {
        warn!(
            started_for = %pending.user_id,
            finishing_as = %user.id,
            "passkey registration finished by a different account than it was started for"
        );
        state
            .security_audit()
            .emit(audit_failure("ceremony_account_mismatch"))
            .await;
        return ceremony_error(StatusCode::BAD_REQUEST, "No registration in progress.");
    }

    let passkey = match state
        .webauthn()
        .finish_passkey_registration(&body.credential, &pending.state)
    {
        Ok(pk) => pk,
        Err(e) => {
            warn!(error = %e, user_id = %user.id, "passkey registration verification failed");
            state
                .security_audit()
                .emit(audit_failure("verification_failed"))
                .await;
            return ceremony_error(
                StatusCode::BAD_REQUEST,
                "Registration could not be verified.",
            );
        }
    };

    let device_name = body
        .device_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());

    let credential =
        match WebauthnCredential::create(state.db(), user.id, &passkey, device_name).await {
            Ok(c) => c,
            Err(e) => {
                // The unique constraint on `credential_id` is load-bearing here: it
                // is how "this authenticator is already registered (possibly to
                // another account)" is detected rather than silently rebound.
                warn!(error = %e, user_id = %user.id, "failed to store registered passkey");
                state
                    .security_audit()
                    .emit(audit_failure("credential_already_registered"))
                    .await;
                return ceremony_error(
                    StatusCode::CONFLICT,
                    "That authenticator is already registered.",
                );
            }
        };

    info!(user_id = %user.id, credential = %credential.id, "passkey registered");
    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::PasskeyRegistered)
                .user(user.id)
                .subject(&credential.credential_id)
                .ip(ip)
                .user_agent(ua)
                .detail("credential_row_id", credential.id.to_string())
                .detail("backup_eligible", credential.backup_eligible),
        )
        .await;

    Json(serde_json::json!({
        "success": true,
        "credential_id": credential.id,
        "device_name": credential.display_name(),
    }))
    .into_response()
}

// ─── Authentication (Story 4.2) ──────────────────────────────────────────────

/// Body of `POST /user/webauthn/login/start`.
#[derive(Deserialize)]
pub struct LoginStartRequest {
    /// The account attempting to authenticate.
    ///
    /// Passkey login is account-scoped here rather than discoverable-credential
    /// based: the allow-list is built from the named account's registered
    /// credentials. See the handler docs on why this is not an enumeration
    /// oracle.
    pub username: String,
}

/// Body of `POST /user/webauthn/login/finish`.
#[derive(Deserialize)]
pub struct LoginFinishRequest {
    /// What `navigator.credentials.get()` returned, serialized.
    pub credential: PublicKeyCredential,
}

/// `POST /user/webauthn/login/start` — begin authenticating with a passkey.
///
/// **Enumeration posture.** An unknown username, a known username with no
/// passkeys, and an inactive account all produce the same generic failure, and
/// all are rate-limited on the same `login` counter as a password attempt. What
/// this endpoint cannot hide is that a *successful* start implies a passkey
/// exists — that is inherent to WebAuthn's allow-list model, and the mitigation
/// is the rate limit, not a pretence.
async fn login_start(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    Json(body): Json<LoginStartRequest>,
) -> Response {
    // Same counter as password login: a passkey ceremony must not be a way
    // around the login rate limit.
    if let Err(retry_after) = state.rate_limiter().check("login", &client_ip.0).await {
        return crate::middleware::rate_limit_response(retry_after);
    }

    let generic = || {
        ceremony_error(
            StatusCode::UNAUTHORIZED,
            "Could not start passkey authentication.",
        )
    };

    let user = match state.users().find_by_name(&body.username).await {
        Ok(Some(u)) if u.is_active() => u,
        Ok(_) => return generic(),
        Err(e) => {
            tracing::error!(error = %e, "database error starting passkey authentication");
            return ceremony_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start passkey authentication.",
            );
        }
    };

    let credentials = match WebauthnCredential::list_for_user(state.db(), user.id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load credentials for authentication");
            return ceremony_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start passkey authentication.",
            );
        }
    };

    let passkeys: Vec<_> = credentials
        .iter()
        .filter_map(|c| c.passkey().ok())
        .collect();
    if passkeys.is_empty() {
        return generic();
    }

    let (challenge, auth_state) = match state.webauthn().start_passkey_authentication(&passkeys) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, user_id = %user.id, "failed to start passkey authentication");
            return generic();
        }
    };

    let pending = PendingAuthentication {
        state: auth_state,
        expires_at: ceremony_expiry(chrono::Utc::now().timestamp()),
        user_id: user.id,
    };
    if let Err(e) = session.insert(SESSION_WEBAUTHN_AUTH, pending).await {
        tracing::error!(error = %e, "failed to store authentication ceremony state");
        return generic();
    }

    Json(challenge).into_response()
}

/// `POST /user/webauthn/login/finish` — verify the assertion and log the user in.
///
/// On success this calls the *existing* `setup_session`, whose first act is
/// `session.cycle_id()`, so the fixation invariant holds for passkey login with
/// no new code on that seam (AC-2), and a passkey login writes the same
/// `SESSION_USER_ID` as a password login — one principal (FINDING-A4).
///
/// D-37 counter regression is handled here: a rejected assertion whose counter
/// did not advance flags the credential and audits, and never auto-revokes.
async fn login_finish(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: HeaderMap,
    Json(body): Json<LoginFinishRequest>,
) -> Response {
    let (ip, ua) = request_context(&headers, &client_ip);

    // Single-use: remove the ceremony before verifying so a replayed `/finish`
    // has nothing to verify against.
    let pending: Option<PendingAuthentication> =
        session.remove(SESSION_WEBAUTHN_AUTH).await.ok().flatten();

    let audit_login_failure = |user_id: Option<Uuid>, reason: &'static str| {
        SecurityEvent::failure(SecurityEventKind::LoginFailed)
            .maybe_user(user_id)
            .ip(ip.clone())
            .user_agent(ua.clone())
            .detail("method", "passkey")
            .detail("reason", reason)
    };

    let Some(pending) = pending else {
        state
            .security_audit()
            .emit(audit_login_failure(None, "no_ceremony_in_progress"))
            .await;
        return ceremony_error(StatusCode::BAD_REQUEST, "No authentication in progress.");
    };

    if !ceremony_is_live(pending.expires_at, chrono::Utc::now().timestamp()) {
        state
            .security_audit()
            .emit(audit_login_failure(
                Some(pending.user_id),
                "ceremony_expired",
            ))
            .await;
        return ceremony_error(
            StatusCode::BAD_REQUEST,
            "Authentication expired. Try again.",
        );
    }

    // Which credential is being asserted, for the D-37 flag and the audit trail.
    let asserted_id = encode_credential_id(body.credential.raw_id.as_ref());
    let stored = WebauthnCredential::find_by_credential_id(state.db(), &asserted_id)
        .await
        .ok()
        .flatten()
        // Scope to the account the ceremony was started for. A credential that
        // belongs to someone else must not be usable to complete this flow.
        .filter(|c| c.user_id == pending.user_id);

    let auth_result = match state
        .webauthn()
        .finish_passkey_authentication(&body.credential, &pending.state)
    {
        Ok(r) => r,
        Err(e) => {
            // D-37: distinguish a counter regression from an ordinary failure.
            // `webauthn-rs` enforces the same rule we state in
            // `counter_regressed` and surfaces it as this variant.
            let regressed = matches!(
                e,
                webauthn_rs::prelude::WebauthnError::CredentialPossibleCompromise
            );
            warn!(
                error = %e,
                user_id = %pending.user_id,
                counter_regression = regressed,
                "passkey authentication failed"
            );

            if regressed && let Some(cred) = stored.as_ref() {
                // Flag, never auto-revoke: a false positive on auto-revoke is a
                // self-inflicted lockout, and a regression has benign causes
                // (authenticator restore) as well as the cloned-key one.
                if let Err(e) = cred
                    .flag(
                        state.db(),
                        "signature counter regressed (possible cloned authenticator)",
                    )
                    .await
                {
                    tracing::error!(error = %e, "failed to flag credential after counter regression");
                }
                state
                    .security_audit()
                    .emit(
                        SecurityEvent::failure(SecurityEventKind::PasskeyCounterRegression)
                            .user(pending.user_id)
                            .subject(&asserted_id)
                            .ip(ip.clone())
                            .user_agent(ua.clone())
                            .detail("stored_sign_count", cred.sign_count)
                            .detail("disposition", "rejected_and_flagged_not_revoked"),
                    )
                    .await;
            }

            state
                .security_audit()
                .emit(audit_login_failure(
                    Some(pending.user_id),
                    if regressed {
                        "counter_regression"
                    } else {
                        "assertion_invalid"
                    },
                ))
                .await;
            return ceremony_error(StatusCode::UNAUTHORIZED, "Authentication failed.");
        }
    };

    let Some(credential) = stored else {
        // The library verified an assertion against a credential we cannot find
        // under this account. Fail closed rather than log anyone in.
        warn!(
            user_id = %pending.user_id,
            "verified assertion for a credential not owned by the ceremony's account"
        );
        state
            .security_audit()
            .emit(audit_login_failure(
                Some(pending.user_id),
                "credential_not_owned",
            ))
            .await;
        return ceremony_error(StatusCode::UNAUTHORIZED, "Authentication failed.");
    };

    // Belt-and-braces on D-37: state the invariant in kernel terms too, so a
    // library-side change in the counter policy cannot silently relax it.
    if counter_regressed(credential.sign_count, i64::from(auth_result.counter())) {
        if let Err(e) = credential
            .flag(
                state.db(),
                "signature counter regressed (possible cloned authenticator)",
            )
            .await
        {
            tracing::error!(error = %e, "failed to flag credential after counter regression");
        }
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::PasskeyCounterRegression)
                    .user(pending.user_id)
                    .subject(&asserted_id)
                    .ip(ip.clone())
                    .user_agent(ua.clone())
                    .detail("stored_sign_count", credential.sign_count)
                    .detail("presented_sign_count", auth_result.counter())
                    .detail("disposition", "rejected_and_flagged_not_revoked"),
            )
            .await;
        state
            .security_audit()
            .emit(audit_login_failure(
                Some(pending.user_id),
                "counter_regression",
            ))
            .await;
        return ceremony_error(StatusCode::UNAUTHORIZED, "Authentication failed.");
    }

    let user = match crate::models::User::find_by_id(state.db(), pending.user_id).await {
        Ok(Some(u)) if u.is_active() => u,
        _ => {
            state
                .security_audit()
                .emit(audit_login_failure(
                    Some(pending.user_id),
                    "account_unavailable",
                ))
                .await;
            return ceremony_error(StatusCode::UNAUTHORIZED, "Authentication failed.");
        }
    };

    if let Err(e) = credential
        .record_authentication(state.db(), &auth_result)
        .await
    {
        // Losing the counter update is a security-relevant degradation (the next
        // regression check compares against a stale value), so it is loud — but
        // the authentication itself already succeeded.
        tracing::error!(error = %e, "failed to record passkey authentication");
    }

    // Clear failed-attempt state and run the same post-login bookkeeping a
    // password login does (this dispatches tap_user_login).
    if let Err(e) = state.lockout().clear_attempts(&user.name).await {
        warn!(error = %e, "failed to clear login attempts after passkey login");
    }
    if let Err(e) = state.users().record_login(&user).await {
        warn!(error = %e, "failed to record login after passkey authentication");
    }

    // AC-2: the one and only session-establishment seam. `setup_session`'s first
    // act is `cycle_id`, so the fixation invariant holds here for free.
    if let Err(e) = crate::routes::auth::setup_session(&session, user.id, false).await {
        tracing::error!(error = %e, "failed to set up session after passkey authentication");
        return ceremony_error(StatusCode::INTERNAL_SERVER_ERROR, "Authentication failed.");
    }

    info!(user_id = %user.id, "user logged in with a passkey");
    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::LoginSucceeded)
                .user(user.id)
                .subject(&asserted_id)
                .ip(ip)
                .user_agent(ua)
                .detail("method", "passkey"),
        )
        .await;

    Json(serde_json::json!({ "success": true })).into_response()
}

// ─── Credential management (Story 4.3) ───────────────────────────────────────

/// One credential as the management page renders it.
#[derive(serde::Serialize)]
struct CredentialView {
    id: String,
    device_name: String,
    created_at: String,
    last_used_at: Option<String>,
    transports: Vec<String>,
    backup_state: bool,
    flagged: bool,
    flag_reason: Option<String>,
}

impl From<&WebauthnCredential> for CredentialView {
    fn from(c: &WebauthnCredential) -> Self {
        Self {
            id: c.id.to_string(),
            device_name: c.display_name(),
            created_at: c.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            last_used_at: c
                .last_used_at
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()),
            transports: c.transports.clone(),
            backup_state: c.backup_state,
            flagged: c.flagged_at.is_some(),
            flag_reason: c.flag_reason.clone(),
        }
    }
}

/// Build the account's current access snapshot for the ≥1-path invariant.
///
/// `non_password_recovery_paths` is whatever the recovery framework reports.
/// Story 4.3 enforces the invariant against whatever exists; Story 4.6 supplies
/// the real paths.
pub(crate) async fn account_access(
    state: &AppState,
    user: &crate::models::User,
) -> crate::services::account_access::AccountAccess {
    let passkey_count = WebauthnCredential::count_for_user(state.db(), user.id)
        .await
        .unwrap_or(0)
        .max(0) as usize;

    crate::services::account_access::AccountAccess {
        has_password: !user.pass.is_empty(),
        passkey_count,
        non_password_recovery_paths: crate::services::account_access::active_recovery_path_count(
            state, user,
        )
        .await,
    }
}

/// Body of the credential rename/revoke endpoints.
#[derive(Deserialize)]
pub struct CredentialActionRequest {
    /// New label, for rename only.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// Maximum length of a user-assigned device label.
const MAX_DEVICE_NAME_LEN: usize = 64;

/// `POST /user/passkeys/{id}/rename` — relabel a credential (AC-1).
async fn rename_credential(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(body): Json<CredentialActionRequest>,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }

    let name = body.device_name.unwrap_or_default();
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_DEVICE_NAME_LEN {
        return ceremony_error(
            StatusCode::BAD_REQUEST,
            "Give the device a name of 1 to 64 characters.",
        );
    }

    // Scoped to the owner, so guessing another account's credential id achieves
    // nothing.
    match WebauthnCredential::rename(state.db(), id, user.id, name).await {
        Ok(true) => {
            state
                .security_audit()
                .emit(
                    SecurityEvent::new(SecurityEventKind::PasskeyRenamed)
                        .user(user.id)
                        .subject(&id.to_string())
                        .ip(client_ip_of(&headers, &state)),
                )
                .await;
            Json(serde_json::json!({ "success": true, "device_name": name })).into_response()
        }
        Ok(false) => ceremony_error(StatusCode::NOT_FOUND, "No such passkey."),
        Err(e) => {
            tracing::error!(error = %e, "failed to rename credential");
            ceremony_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not rename the passkey.",
            )
        }
    }
}

/// The IP for an audit event when the handler has no `ClientIp` extractor.
///
/// Management endpoints are already login-gated, so the IP is context rather
/// than a security decision; falling back to the header keeps the audit row
/// useful without adding an extractor to every signature.
fn client_ip_of(headers: &HeaderMap, _state: &AppState) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// `POST /user/passkeys/{id}/revoke` — remove a credential (AC-2, AC-3).
///
/// State-changing, so POST + `require_csrf`. The removal is gated by the
/// ≥1-active-recovery-path invariant (D-33): revoking the **last** way into the
/// account is refused, with the reason named, rather than silently permitted and
/// then papered over with a password fallback.
async fn revoke_credential(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    headers: HeaderMap,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }
    let ip = client_ip_of(&headers, &state);

    // Confirm ownership before doing anything else, so a bad id is a 404 rather
    // than an invariant question about someone else's account.
    match WebauthnCredential::find_owned(state.db(), id, user.id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ceremony_error(StatusCode::NOT_FOUND, "No such passkey."),
        Err(e) => {
            tracing::error!(error = %e, "failed to load credential for revocation");
            return ceremony_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not remove the passkey.",
            );
        }
    }

    let access = account_access(&state, &user).await;
    if let Err(blocked) = access.can_remove_passkey() {
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::CredentialRemovalBlocked)
                    .user(user.id)
                    .subject(&id.to_string())
                    .ip(ip)
                    .detail("removing", "passkey")
                    .detail("reason", blocked.as_str())
                    .detail("passkey_count", access.passkey_count)
                    .detail("has_password", access.has_password)
                    .detail("recovery_paths", access.non_password_recovery_paths),
            )
            .await;
        return ceremony_error(StatusCode::CONFLICT, blocked.message());
    }

    match WebauthnCredential::delete_owned(state.db(), id, user.id).await {
        Ok(true) => {
            info!(user_id = %user.id, credential = %id, "passkey revoked");
            state
                .security_audit()
                .emit(
                    SecurityEvent::new(SecurityEventKind::PasskeyRevoked)
                        .user(user.id)
                        .subject(&id.to_string())
                        .ip(ip)
                        .detail("remaining_passkeys", access.passkey_count.saturating_sub(1)),
                )
                .await;
            Json(serde_json::json!({ "success": true })).into_response()
        }
        Ok(false) => ceremony_error(StatusCode::NOT_FOUND, "No such passkey."),
        Err(e) => {
            tracing::error!(error = %e, "failed to revoke credential");
            ceremony_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not remove the passkey.",
            )
        }
    }
}

/// `POST /user/password/remove` — go passwordless (D-33).
///
/// The other half of the invariant, and the one with the real friction: a
/// password may only be dropped when a passkey **and** a non-password recovery
/// path both exist. There is deliberately no way to force it.
async fn remove_password(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }
    let ip = client_ip_of(&headers, &state);

    if user.pass.is_empty() {
        return Json(serde_json::json!({
            "success": true,
            "message": "This account already has no password."
        }))
        .into_response();
    }

    let access = account_access(&state, &user).await;
    if let Err(blocked) = access.can_remove_password() {
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::CredentialRemovalBlocked)
                    .user(user.id)
                    .ip(ip)
                    .detail("removing", "password")
                    .detail("reason", blocked.as_str())
                    .detail("passkey_count", access.passkey_count)
                    .detail("recovery_paths", access.non_password_recovery_paths),
            )
            .await;
        return ceremony_error(StatusCode::CONFLICT, blocked.message());
    }

    if let Err(e) = sqlx::query("UPDATE users SET pass = '' WHERE id = $1")
        .bind(user.id)
        .execute(state.db())
        .await
    {
        tracing::error!(error = %e, "failed to remove password");
        return ceremony_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not remove the password.",
        );
    }
    state.users().invalidate(user.id);

    // An auth-state change, so the fixation defence fires here too.
    if let Err(e) = session.cycle_id().await {
        warn!(error = %e, "failed to cycle session after going passwordless");
    }

    info!(user_id = %user.id, "account went passwordless");
    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::PasswordRemoved)
                .user(user.id)
                .ip(ip)
                .detail("passkey_count", access.passkey_count)
                .detail("recovery_paths", access.non_password_recovery_paths),
        )
        .await;

    Json(serde_json::json!({ "success": true })).into_response()
}

// ─── The SSR entry page ──────────────────────────────────────────────────────

/// `GET /user/passkeys` — the passkey management page.
///
/// The SSR half of "one ceremony, two entry pages": this renders the credential
/// list and ships the glue JS that calls the same JSON endpoints a pure-API
/// client would, rather than duplicating ceremony logic.
async fn passkeys_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let csrf_token = crate::form::csrf::generate_csrf_token(&session).await;

    let credentials = WebauthnCredential::list_for_user(state.db(), user.id)
        .await
        .unwrap_or_default();
    let views: Vec<CredentialView> = credentials.iter().map(CredentialView::from).collect();

    let access = account_access(&state, &user).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("user", &user);
    context.insert("credentials", &views);
    context.insert("has_password", &access.has_password);
    context.insert("recovery_paths", &access.non_password_recovery_paths);
    // Pre-computed so the template states the outcome rather than re-deriving
    // the invariant in Tera, where it could drift from the enforced rule.
    context.insert("can_go_passwordless", &access.can_remove_password().is_ok());
    context.insert(
        "passwordless_blocker",
        &access.can_remove_password().err().map(|b| b.message()),
    );
    crate::routes::helpers::inject_site_context(&state, &session, &mut context, "/user/passkeys")
        .await;

    match state.theme().tera().render("user/passkeys.html", &context) {
        Ok(html) => axum::response::Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the passkeys page");
            crate::routes::helpers::render_server_error("Could not render the passkeys page.")
        }
    }
}

/// The WebAuthn ceremony router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/passkeys", axum::routing::get(passkeys_page))
        .route("/user/webauthn/register/start", post(register_start))
        .route("/user/webauthn/register/finish", post(register_finish))
        .route("/user/webauthn/login/start", post(login_start))
        .route("/user/webauthn/login/finish", post(login_finish))
        .route("/user/passkeys/{id}/rename", post(rename_credential))
        .route("/user/passkeys/{id}/revoke", post(revoke_credential))
        .route("/user/password/remove", post(remove_password))
}
