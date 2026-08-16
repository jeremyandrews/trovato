//! Account-recovery HTTP flow (FR-7c, design §4.4, Story 4.6).
//!
//! The kernel drives every step; a plugin only answers the three ops the frozen
//! tap allows. The state machine is exactly design §4.4:
//!
//! ```text
//! Idle
//!  → POST /user/recover/start    rate-limit, bind the account, mint the nonce,
//!                                dispatch `describe`, present the chooser
//!  → POST /user/recover/choose   dispatch `initiate` for one method
//!  → POST /user/recover/verify   dispatch `verify`, apply the frozen fold
//!  → RecoveryAuthenticated       a SCOPED credential-reset grant (D-38)
//!  → POST /user/recover/reset    set the new credential, then setup_session
//!                                (so cycle_id fires) and burn the nonce
//! ```
//!
//! # Why a plugin cannot skip a step
//!
//! Each transition is gated on kernel state a plugin never sees: the flow nonce
//! is kernel-minted and stored server-side, `choose` requires a flow in
//! `Started`, `verify` requires one in `AwaitingVerification` with the method it
//! was initiated for, and `reset` requires the scoped grant that only a folded
//! `Granted` produces. A plugin returning `Verified` to a `describe`, or to a
//! `verify` for a method the flow never chose, moves nothing — there is no
//! transition it can reach without the kernel having already made it.
//!
//! # Enumeration
//!
//! `start` answers identically for a known account, an unknown one, and a
//! blocked one, and is rate-limited per-IP before it looks anything up. What the
//! method chooser reveals, it reveals only *after* the kernel's own gate.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_sessions::Session;
use tracing::{info, warn};
use uuid::Uuid;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::recovery::{
    RecoveryTapInput, RecoveryTapResult, RecoveryVerifyOutcome, Verdict, fold_recovery_verify,
};
use crate::routes::helpers::{render_server_error, require_csrf_header, validate_password};
use crate::services::recovery_flow::{
    FlowState, GRANT_TTL_SECS, RecoveryFlow, RecoveryGrant, SESSION_RECOVERY_FLOW,
    SESSION_RECOVERY_GRANT, account_of, collect_methods, dispatch_builtins, owns_method,
};
use crate::state::AppState;
use crate::tap::TapResult;

/// The one response an unauthenticated recovery initiation ever gets.
///
/// Constant-shape and account-independent: a known account, an unknown one, a
/// blocked one, and one with no methods all produce this. Anything else would
/// make the endpoint an account-existence oracle.
const GENERIC_START_MESSAGE: &str =
    "If that account exists and has a recovery method configured, we have started the process.";

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The client IP and User-Agent for the audit stream.
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

/// How many non-password recovery paths an account currently has.
///
/// This is the real implementation of the hook Story 4.3 left in
/// `services::account_access`: it asks every provider, built-in and plugin,
/// what it offers for this account, and counts the methods reported available.
/// The ≥1-active-recovery-path invariant is therefore enforced against what the
/// account can *actually* use, not against a static list.
pub async fn active_recovery_path_count(state: &AppState, user: &crate::models::User) -> usize {
    let input = RecoveryTapInput::Describe {
        // A synthetic nonce: this is a capability question, not a live flow, and
        // no plugin should be able to tell the difference or act on it.
        flow_id: Uuid::new_v4().to_string(),
        account: crate::recovery::RecoveryAccount {
            user_id: user.id,
            email_present: !user.mail.trim().is_empty(),
        },
        locale: None,
    };

    let results = dispatch_recovery(state, &input).await;
    collect_methods(&results)
        .into_iter()
        .filter(|m| m.available)
        .count()
}

/// Dispatch one recovery op to **every** implementer — built-in and WASM — and
/// return their answers in the one uniform form the fold consumes.
///
/// This is the single dispatch site. Built-in results are synthesized into
/// `TapResult`s carrying their reserved plugin name, so the fold's owner check
/// applies to them exactly as it does to a plugin's.
async fn dispatch_recovery(state: &AppState, input: &RecoveryTapInput) -> Vec<TapResult> {
    let mut results = dispatch_builtins(state.recovery_providers(), input).await;

    let Ok(input_json) = serde_json::to_string(input) else {
        tracing::error!("failed to encode a recovery tap input; dispatching built-ins only");
        return results;
    };

    let request_state = crate::tap::RequestState::new(
        // Recovery runs for an as-yet-unauthenticated caller; the plugin gets
        // the anonymous principal and learns the account only through the
        // frozen payload's `account` field.
        crate::tap::UserContext::anonymous(),
        state.tap_services().clone(),
    );
    let plugin_results = state
        .tap_dispatcher()
        .dispatch("tap_account_recovery", &input_json, request_state)
        .await;

    results.extend(plugin_results);
    results
}

/// Emit the D-32 fold-audit amendment as durable events (AC-6).
///
/// Story 4.5 froze the fold and surfaced these as `tracing` warnings as the
/// interim honest form of "never silently dropped". This upgrades them to
/// structured audit events now that the module exists. **The fold semantics are
/// unchanged** — nothing frozen moves; this is purely an observability side
/// effect on the paths the fold already ignores.
async fn audit_fold_anomalies(
    state: &AppState,
    results: &[TapResult],
    method_id: &str,
    user_id: Uuid,
    ip: &str,
) {
    for result in results {
        let is_owner = owns_method(&result.plugin_name, method_id);
        match serde_json::from_str::<RecoveryTapResult>(&result.output) {
            Ok(RecoveryTapResult::Verdict { verdict }) => {
                if !is_owner && matches!(verdict, Verdict::Verified) {
                    // A plugin forged `Verified` for a namespace it does not
                    // own: an attempted account escalation. Alert-worthy.
                    warn!(
                        plugin = %result.plugin_name,
                        method_id = %method_id,
                        "recovery fold: ignored a forged Verified from a non-owner (attempted escalation)"
                    );
                    state
                        .security_audit()
                        .emit(
                            SecurityEvent::failure(
                                SecurityEventKind::RecoveryFoldEscalationAttempt,
                            )
                            .user(user_id)
                            .ip(ip.to_string())
                            .detail("plugin", result.plugin_name.clone())
                            .detail("method_id", method_id)
                            .detail("forged_verdict", "Verified"),
                        )
                        .await;
                }
            }
            Ok(_wrong_shape) => {
                if is_owner {
                    state
                        .security_audit()
                        .emit(
                            SecurityEvent::failure(SecurityEventKind::RecoveryFoldWrongShape)
                                .user(user_id)
                                .ip(ip.to_string())
                                .detail("plugin", result.plugin_name.clone())
                                .detail("method_id", method_id)
                                .detail("problem", "non_verdict_result_shape"),
                        )
                        .await;
                }
            }
            Err(_unparseable) => {
                if is_owner {
                    state
                        .security_audit()
                        .emit(
                            SecurityEvent::failure(SecurityEventKind::RecoveryFoldWrongShape)
                                .user(user_id)
                                .ip(ip.to_string())
                                .detail("plugin", result.plugin_name.clone())
                                .detail("method_id", method_id)
                                .detail("problem", "unparseable_output"),
                        )
                        .await;
                }
            }
        }
    }
}

/// Notify every channel we have on the account (design §5).
///
/// A silent takeover is the failure mode recovery introduces, so initiation and
/// completion are both announced. Best-effort: an unsendable notification must
/// not fail the flow, but it is logged, because a recovery nobody could be told
/// about is worth knowing happened.
async fn notify(state: &AppState, user_id: Uuid, subject: &str, body: &str) {
    let Some(email_service) = state.email() else {
        return;
    };
    let Ok(Some(user)) = crate::models::User::find_by_id(state.db(), user_id).await else {
        return;
    };
    if user.mail.trim().is_empty() {
        return;
    }
    if let Err(e) = email_service.send(&user.mail, subject, body).await {
        warn!(error = %e, "failed to send a recovery notification");
    }
}

// ─── The flow ────────────────────────────────────────────────────────────────

/// `GET /user/recover` — the recovery entry page.
async fn recover_page(State(state): State<AppState>, session: Session) -> Response {
    let csrf_token = crate::form::csrf::generate_csrf_token(&session).await;
    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    crate::routes::helpers::inject_site_context(&state, &session, &mut context, "/user/recover")
        .await;

    match state.theme().tera().render("user/recover.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the recovery page");
            render_server_error("Could not render the recovery page.")
        }
    }
}

/// Body of `POST /user/recover/start`.
#[derive(Deserialize)]
pub struct StartRequest {
    /// The account name or email address to recover.
    pub identifier: String,
}

/// `POST /user/recover/start` — bind an account and offer the methods (AC-1, AC-4).
async fn recover_start(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: HeaderMap,
    Json(body): Json<StartRequest>,
) -> Response {
    let (ip, ua) = request_context(&headers, &client_ip);

    // Per-IP first, before any lookup, so a spray cannot use the lookup itself
    // as an oracle even at the rate-limit boundary.
    if let Err(retry_after) = state.rate_limiter().check("recovery", &ip).await {
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::RecoveryRateLimited)
                    .ip(ip.clone())
                    .user_agent(ua.clone())
                    .detail("scope", "ip"),
            )
            .await;
        return crate::middleware::rate_limit_response(retry_after);
    }

    let generic = || {
        Json(serde_json::json!({
            "success": true,
            "message": GENERIC_START_MESSAGE,
            "methods": Vec::<serde_json::Value>::new(),
        }))
        .into_response()
    };

    let identifier = body.identifier.trim();
    if identifier.is_empty() {
        return generic();
    }

    // Accept either a username or an email address; both resolve to one account
    // and both produce the same response shape when they resolve to none.
    let user = match state.users().find_by_name(identifier).await {
        Ok(Some(u)) => Some(u),
        _ => crate::models::User::find_by_mail(state.db(), identifier)
            .await
            .ok()
            .flatten(),
    };

    let Some(user) = user.filter(|u| u.is_active()) else {
        return generic();
    };

    // Per-account as well as per-IP (design §5): per-IP stops broad spraying,
    // per-account stops a distributed flood aimed at one victim's inbox.
    if let Err(retry_after) = state
        .rate_limiter()
        .check("recovery", &format!("user:{}", user.id))
        .await
    {
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::RecoveryRateLimited)
                    .user(user.id)
                    .ip(ip.clone())
                    .user_agent(ua.clone())
                    .detail("scope", "account"),
            )
            .await;
        // Still generic to the caller: revealing that THIS account is being
        // throttled would confirm it exists.
        let _ = retry_after;
        return generic();
    }

    let now = chrono::Utc::now().timestamp();
    let email_present = !user.mail.trim().is_empty();
    let flow = match state
        .recovery_flows()
        .start(user.id, email_present, now)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "failed to start a recovery flow");
            return generic();
        }
    };

    // The flow nonce lives in the session, not in the response: the client never
    // needs to hold it, and not handing it out removes a whole class of nonce
    // sharing/forwarding mistakes.
    if let Err(e) = session.insert(SESSION_RECOVERY_FLOW, &flow.flow_id).await {
        tracing::error!(error = %e, "failed to store the recovery flow id");
        return generic();
    }

    let input = RecoveryTapInput::Describe {
        flow_id: flow.flow_id.clone(),
        account: account_of(&flow),
        locale: None,
    };
    let results = dispatch_recovery(&state, &input).await;
    let methods: Vec<_> = collect_methods(&results)
        .into_iter()
        .filter(|m| m.available)
        .collect();

    info!(user_id = %user.id, methods = methods.len(), "recovery initiated");
    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::RecoveryInitiated)
                .user(user.id)
                .subject(&flow.flow_id)
                .ip(ip)
                .user_agent(ua)
                .detail("methods_offered", methods.len()),
        )
        .await;

    notify(
        &state,
        user.id,
        "Account recovery was requested",
        "Someone started the account-recovery process for your account.\n\n\
         If this was not you, no action has been taken yet, but someone knows or \
         guessed your account name — consider reviewing your sign-in methods.\n",
    )
    .await;

    Json(serde_json::json!({
        "success": true,
        "message": GENERIC_START_MESSAGE,
        "methods": methods,
    }))
    .into_response()
}

/// Load the caller's live flow, or explain why there is not one.
async fn load_flow(
    state: &AppState,
    session: &Session,
    now: i64,
) -> Result<RecoveryFlow, Response> {
    let Some(flow_id): Option<String> = session.get(SESSION_RECOVERY_FLOW).await.ok().flatten()
    else {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "No recovery in progress.",
        ));
    };

    match state.recovery_flows().get(&flow_id, now).await {
        Ok(Some(flow)) => Ok(flow),
        Ok(None) => Err(json_error(
            StatusCode::BAD_REQUEST,
            "That recovery attempt has expired. Start again.",
        )),
        Err(e) => {
            tracing::error!(error = %e, "failed to load the recovery flow");
            Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not continue the recovery.",
            ))
        }
    }
}

/// Body of `POST /user/recover/choose`.
#[derive(Deserialize)]
pub struct ChooseRequest {
    /// The namespaced method the user picked.
    pub method_id: String,
}

/// `POST /user/recover/choose` — start one method's challenge (AC-1).
async fn recover_choose(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: HeaderMap,
    Json(body): Json<ChooseRequest>,
) -> Response {
    let (ip, ua) = request_context(&headers, &client_ip);
    let now = chrono::Utc::now().timestamp();

    let mut flow = match load_flow(&state, &session, now).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };

    // A method may only be chosen from `Started`. Re-choosing after a challenge
    // has begun would let a caller mint challenges without limit.
    if flow.state != FlowState::Started {
        return json_error(
            StatusCode::CONFLICT,
            "A recovery method has already been started.",
        );
    }

    let input = RecoveryTapInput::Initiate {
        flow_id: flow.flow_id.clone(),
        account: account_of(&flow),
        method_id: body.method_id.clone(),
    };
    let results = dispatch_recovery(&state, &input).await;

    // Only the OWNER's answer counts, checked here rather than left to the fold:
    // a non-owner's `initiated` must not be what advances the flow.
    let initiated = results.iter().find_map(|r| {
        if !owns_method(&r.plugin_name, &body.method_id) {
            return None;
        }
        match serde_json::from_str::<RecoveryTapResult>(&r.output) {
            Ok(RecoveryTapResult::Initiated {
                status,
                challenge_hint,
                ..
            }) if status == "initiated" => Some(challenge_hint),
            _ => None,
        }
    });

    let Some(challenge_hint) = initiated else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "That recovery method is not available.",
        );
    };

    flow.state = FlowState::AwaitingVerification;
    flow.method_id = Some(body.method_id.clone());
    if let Err(e) = state.recovery_flows().put(&flow).await {
        tracing::error!(error = %e, "failed to advance the recovery flow");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not continue the recovery.",
        );
    }

    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::RecoveryMethodInitiated)
                .user(flow.user_id)
                .subject(&flow.flow_id)
                .ip(ip)
                .user_agent(ua)
                .detail("method_id", body.method_id),
        )
        .await;

    Json(serde_json::json!({
        "success": true,
        "challenge_hint": challenge_hint,
    }))
    .into_response()
}

/// Body of `POST /user/recover/verify`.
#[derive(Deserialize)]
pub struct VerifyRequest {
    /// Whatever the user submitted; opaque to the kernel.
    pub response: String,
}

/// `POST /user/recover/verify` — fold the verdicts and, on success, grant the
/// scoped credential-reset state (AC-1, AC-6, D-32, D-38).
async fn recover_verify(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: HeaderMap,
    Json(body): Json<VerifyRequest>,
) -> Response {
    let (ip, ua) = request_context(&headers, &client_ip);
    let now = chrono::Utc::now().timestamp();

    // Submissions are rate-limited too: without this, the flow TTL would be a
    // brute-force window against a short code.
    if let Err(retry_after) = state.rate_limiter().check("recovery", &ip).await {
        state
            .security_audit()
            .emit(
                SecurityEvent::failure(SecurityEventKind::RecoveryRateLimited)
                    .ip(ip.clone())
                    .user_agent(ua.clone())
                    .detail("scope", "ip")
                    .detail("stage", "verify"),
            )
            .await;
        return crate::middleware::rate_limit_response(retry_after);
    }

    let flow = match load_flow(&state, &session, now).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };

    // Verification is only reachable from `AwaitingVerification`, for the method
    // that was actually initiated. This is the step a hostile plugin would want
    // to skip, and it is enforced entirely on kernel state.
    if flow.state != FlowState::AwaitingVerification {
        return json_error(
            StatusCode::CONFLICT,
            "Choose a recovery method before verifying.",
        );
    }
    let Some(method_id) = flow.method_id.clone() else {
        return json_error(
            StatusCode::CONFLICT,
            "Choose a recovery method before verifying.",
        );
    };

    let input = RecoveryTapInput::Verify {
        flow_id: flow.flow_id.clone(),
        account: account_of(&flow),
        method_id: method_id.clone(),
        response: body.response.clone(),
    };
    let results = dispatch_recovery(&state, &input).await;

    // The D-32 amendment: surface what the fold is about to ignore, before
    // folding. The fold's own semantics are untouched.
    audit_fold_anomalies(&state, &results, &method_id, flow.user_id, &ip).await;

    // The frozen fold, unchanged from Story 4.5.
    let outcome = fold_recovery_verify(&results, &method_id);

    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::RecoveryVerdict)
                .user(flow.user_id)
                .subject(&flow.flow_id)
                .ip(ip.clone())
                .user_agent(ua.clone())
                .detail("method_id", method_id.clone())
                .detail("outcome", format!("{outcome:?}")),
        )
        .await;

    match outcome {
        RecoveryVerifyOutcome::Granted => {
            // D-38: a SCOPED credential-reset state, never a session. Note what
            // is deliberately absent — no SESSION_USER_ID is written here.
            let grant = RecoveryGrant {
                user_id: flow.user_id,
                flow_id: flow.flow_id.clone(),
                expires_at: now + GRANT_TTL_SECS,
            };
            if let Err(e) = session.insert(SESSION_RECOVERY_GRANT, &grant).await {
                tracing::error!(error = %e, "failed to store the recovery grant");
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not complete the recovery.",
                );
            }

            info!(user_id = %flow.user_id, "recovery verified; scoped reset granted");
            Json(serde_json::json!({
                "success": true,
                "next": "reset",
                "message": "Verified. Set a new password to finish.",
            }))
            .into_response()
        }
        RecoveryVerifyOutcome::Pending => Json(serde_json::json!({
            "success": true,
            "next": "wait",
            "message": "Waiting for approval. Try again shortly.",
        }))
        .into_response(),
        RecoveryVerifyOutcome::Rejected | RecoveryVerifyOutcome::Denied => {
            // Burn the nonce on a terminal failure: single-use in both
            // directions, so a rejected flow cannot be retried indefinitely
            // inside its TTL.
            if let Err(e) = state.recovery_flows().burn(&flow.flow_id).await {
                warn!(error = %e, "failed to burn a rejected recovery flow");
            }
            let _ = session.remove::<String>(SESSION_RECOVERY_FLOW).await;
            json_error(StatusCode::UNAUTHORIZED, "That did not verify.")
        }
    }
}

/// Body of `POST /user/recover/reset`.
#[derive(Deserialize)]
pub struct ResetRequest {
    /// The new password to set on the recovered account.
    pub new_password: String,
}

/// `POST /user/recover/reset` — spend the scoped grant (AC-1, D-38).
///
/// This is the only thing a recovery success authorizes. Afterwards the normal
/// session is established through `setup_session`, so `cycle_id` fires, and the
/// flow nonce is burned.
async fn recover_reset(
    State(state): State<AppState>,
    session: Session,
    axum::Extension(client_ip): axum::Extension<crate::middleware::ClientIp>,
    headers: HeaderMap,
    Json(body): Json<ResetRequest>,
) -> Response {
    let (ip, ua) = request_context(&headers, &client_ip);
    let now = chrono::Utc::now().timestamp();

    let Some(grant): Option<RecoveryGrant> =
        session.get(SESSION_RECOVERY_GRANT).await.ok().flatten()
    else {
        return json_error(StatusCode::FORBIDDEN, "No verified recovery to complete.");
    };
    if !grant.is_live(now) {
        let _ = session
            .remove::<RecoveryGrant>(SESSION_RECOVERY_GRANT)
            .await;
        return json_error(StatusCode::FORBIDDEN, "That recovery expired. Start again.");
    }

    if let Err(message) = validate_password(&body.new_password) {
        return json_error(StatusCode::BAD_REQUEST, message);
    }

    let user_ctx = crate::tap::UserContext::authenticated(grant.user_id, vec![]);
    match state
        .users()
        .update_password(grant.user_id, &body.new_password, &user_ctx)
        .await
    {
        Ok(true) => {}
        _ => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not set the new password.",
            );
        }
    }

    // Spend the grant and burn the nonce: both single-use.
    let _ = session
        .remove::<RecoveryGrant>(SESSION_RECOVERY_GRANT)
        .await;
    let _ = session.remove::<String>(SESSION_RECOVERY_FLOW).await;
    if let Err(e) = state.recovery_flows().burn(&grant.flow_id).await {
        warn!(error = %e, "failed to burn a completed recovery flow");
    }

    // Only now does a real session exist, and it is established through the one
    // seam, so `cycle_id` fires (D-38's closing clause).
    if let Err(e) = crate::routes::auth::setup_session(&session, grant.user_id, false).await {
        tracing::error!(error = %e, "failed to establish a session after recovery");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Password set, but sign-in failed. Try signing in.",
        );
    }

    info!(user_id = %grant.user_id, "recovery completed");
    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::RecoveryCompleted)
                .user(grant.user_id)
                .subject(&grant.flow_id)
                .ip(ip)
                .user_agent(ua)
                .detail("granted", "scoped_credential_reset"),
        )
        .await;

    notify(
        &state,
        grant.user_id,
        "Your account was recovered",
        "Your account was just recovered and its password was changed.\n\n\
         If this was not you, act now: your account has been taken over.\n",
    )
    .await;

    Json(serde_json::json!({ "success": true })).into_response()
}

// ─── Recovery-code self-service ──────────────────────────────────────────────

/// `POST /user/recovery-codes/generate` — issue a fresh batch (AC-2).
///
/// Login-gated: generating codes is an operation on your own account, and the
/// plaintext is shown exactly once.
async fn generate_recovery_codes(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
) -> Response {
    let user = match crate::routes::helpers::require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }

    match crate::services::recovery_builtins::RecoveryCodesProvider::generate(state.db(), user.id)
        .await
    {
        Ok(codes) => {
            info!(user_id = %user.id, count = codes.len(), "recovery codes generated");
            Json(serde_json::json!({ "success": true, "codes": codes })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to generate recovery codes");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not generate recovery codes.",
            )
        }
    }
}

// ─── Admin configuration (AC-5) ──────────────────────────────────────────────

/// Body of the admin recovery-configuration endpoint.
#[derive(Deserialize)]
pub struct RecoveryConfigRequest {
    /// Enable the built-in emailed-code path.
    pub email_enabled: bool,
    /// Enable the built-in pre-generated-code path.
    pub codes_enabled: bool,
}

/// `GET /admin/recovery` — the recovery configuration page.
async fn admin_recovery_page(State(state): State<AppState>, session: Session) -> Response {
    if let Err(resp) = crate::routes::helpers::require_admin(&state, &session).await {
        return resp;
    }

    let email_enabled = crate::models::SiteConfig::get(
        state.db(),
        crate::services::recovery_flow::CONFIG_EMAIL_ENABLED,
    )
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_bool())
    .unwrap_or(true);
    let codes_enabled = crate::models::SiteConfig::get(
        state.db(),
        crate::services::recovery_flow::CONFIG_CODES_ENABLED,
    )
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_bool())
    .unwrap_or(true);

    let csrf_token = crate::form::csrf::generate_csrf_token(&session).await;
    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("email_enabled", &email_enabled);
    context.insert("codes_enabled", &codes_enabled);
    // An admin needs to know that email recovery is the weakest link, at the
    // moment they are deciding whether to leave it on.
    context.insert("smtp_configured", &state.email().is_some());
    crate::routes::helpers::inject_site_context(&state, &session, &mut context, "/admin/recovery")
        .await;

    match state.theme().tera().render("admin/recovery.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the recovery admin page");
            render_server_error("Could not render the recovery settings.")
        }
    }
}

/// `POST /admin/recovery` — save the recovery configuration.
async fn admin_recovery_save(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<RecoveryConfigRequest>,
) -> Response {
    if let Err(resp) = crate::routes::helpers::require_admin(&state, &session).await {
        return resp;
    }
    if let Err((status, resp)) = require_csrf_header(&session, &headers).await {
        return (status, resp).into_response();
    }

    for (key, value) in [
        (
            crate::services::recovery_flow::CONFIG_EMAIL_ENABLED,
            body.email_enabled,
        ),
        (
            crate::services::recovery_flow::CONFIG_CODES_ENABLED,
            body.codes_enabled,
        ),
    ] {
        if let Err(e) =
            crate::models::SiteConfig::set(state.db(), key, serde_json::Value::Bool(value)).await
        {
            tracing::error!(error = %e, key = key, "failed to save a recovery setting");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not save the recovery settings.",
            );
        }
    }

    Json(serde_json::json!({ "success": true })).into_response()
}

/// The recovery router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/recover", get(recover_page))
        .route("/user/recover/start", post(recover_start))
        .route("/user/recover/choose", post(recover_choose))
        .route("/user/recover/verify", post(recover_verify))
        .route("/user/recover/reset", post(recover_reset))
        .route(
            "/user/recovery-codes/generate",
            post(generate_recovery_codes),
        )
        .route(
            "/admin/recovery",
            get(admin_recovery_page).post(admin_recovery_save),
        )
}
