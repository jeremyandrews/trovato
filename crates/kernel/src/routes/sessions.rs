//! Multi-device session management (FR-7b, design §3, Story 4.4).
//!
//! Enumerate, rename, and revoke the sessions of an account — for the user
//! themselves, and for an admin over any account. Everything reads and writes
//! the Redis per-user index in [`crate::services::session_registry`]; nothing
//! here touches the tower-sessions cookie, expiry, or CSRF semantics.
//!
//! Revocation addresses a **device id**, not a session id, so the action a user
//! takes ("log this laptop out") stays meaningful across a `cycle_id` that
//! changed the underlying session id.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use tracing::{info, warn};
use uuid::Uuid;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::routes::helpers::{
    render_not_found, render_server_error, require_admin, require_csrf_header, require_login,
};
use crate::services::session_registry::{SESSION_DEVICE_ID, SessionEntry};
use crate::state::AppState;

/// Maximum length of a user-assigned device label.
const MAX_DEVICE_NAME_LEN: usize = 64;

/// One session as the management page renders it.
#[derive(Serialize)]
struct SessionView {
    device_id: String,
    device_name: String,
    user_agent: String,
    ip: String,
    created_at: String,
    last_seen: String,
    /// Whether this is the session making the request. The UI marks it so a
    /// user does not log themselves out by mistake.
    is_current: bool,
}

/// Format a unix timestamp for display.
fn format_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn to_view(entry: &SessionEntry, current_device: Option<Uuid>) -> SessionView {
    SessionView {
        device_id: entry.device_id.to_string(),
        device_name: entry.device_name.clone(),
        user_agent: entry.user_agent.clone(),
        ip: entry.ip.clone(),
        created_at: format_ts(entry.created_at),
        last_seen: format_ts(entry.last_seen),
        is_current: current_device == Some(entry.device_id),
    }
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The client IP for an audit event on a login-gated management endpoint.
fn client_ip_of(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

// ─── User self-service ───────────────────────────────────────────────────────

/// `GET /user/sessions` — the caller's active sessions (AC-2).
async fn sessions_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let current_device = session.get::<Uuid>(SESSION_DEVICE_ID).await.ok().flatten();
    let entries = state
        .session_registry()
        .list(user.id)
        .await
        .unwrap_or_default();
    let views: Vec<SessionView> = entries.iter().map(|e| to_view(e, current_device)).collect();

    let csrf_token = crate::form::csrf::generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("user", &user);
    context.insert("sessions", &views);
    context.insert("is_admin_view", &false);
    crate::routes::helpers::inject_site_context(&state, &session, &mut context, "/user/sessions")
        .await;

    match state.theme().tera().render("user/sessions.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the sessions page");
            render_server_error("Could not render the sessions page.")
        }
    }
}

/// Body of the session rename endpoint.
#[derive(Deserialize)]
pub struct RenameSessionRequest {
    /// The new label.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// `POST /user/sessions/{device_id}/rename` — relabel a device (AC-1).
async fn rename_session(
    State(state): State<AppState>,
    session: Session,
    Path(device_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<RenameSessionRequest>,
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
        return json_error(
            StatusCode::BAD_REQUEST,
            "Give the device a name of 1 to 64 characters.",
        );
    }

    match state
        .session_registry()
        .rename(user.id, device_id, name)
        .await
    {
        Ok(true) => Json(serde_json::json!({ "success": true })).into_response(),
        Ok(false) => json_error(StatusCode::NOT_FOUND, "No such session."),
        Err(e) => {
            tracing::error!(error = %e, "failed to rename a session");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not rename the session.",
            )
        }
    }
}

/// `POST /user/sessions/{device_id}/revoke` — remote logout (AC-2).
async fn revoke_session(
    State(state): State<AppState>,
    session: Session,
    Path(device_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }
    let ip = client_ip_of(&headers);

    // Scoped to the caller's own user id, so a device id belonging to another
    // account is simply not found.
    match state.session_registry().revoke(user.id, device_id).await {
        Ok(Some(entry)) => {
            info!(user_id = %user.id, device = %device_id, "session revoked by user");
            state
                .security_audit()
                .emit(
                    SecurityEvent::new(SecurityEventKind::SessionRevokedByUser)
                        .user(user.id)
                        .subject(&entry.session_id)
                        .ip(ip)
                        .user_agent(entry.user_agent.clone())
                        .detail("device_id", device_id.to_string())
                        .detail("device_name", entry.device_name),
                )
                .await;
            Json(serde_json::json!({ "success": true })).into_response()
        }
        Ok(None) => json_error(StatusCode::NOT_FOUND, "No such session."),
        Err(e) => {
            tracing::error!(error = %e, "failed to revoke a session");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not revoke the session.",
            )
        }
    }
}

/// `POST /user/sessions/revoke-others` — log out everywhere else (AC-2).
///
/// Deliberately keeps the caller's own session: the point is to evict everything
/// *else*, and logging the user out of the page they clicked from would make the
/// action self-defeating.
async fn revoke_other_sessions(
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
    let ip = client_ip_of(&headers);
    let current_device = session.get::<Uuid>(SESSION_DEVICE_ID).await.ok().flatten();

    match state
        .session_registry()
        .revoke_all_except(user.id, current_device)
        .await
    {
        Ok(revoked) => {
            info!(user_id = %user.id, count = revoked.len(), "other sessions revoked by user");
            for entry in &revoked {
                state
                    .security_audit()
                    .emit(
                        SecurityEvent::new(SecurityEventKind::SessionRevokedByUser)
                            .user(user.id)
                            .subject(&entry.session_id)
                            .ip(ip.clone())
                            .user_agent(entry.user_agent.clone())
                            .detail("device_id", entry.device_id.to_string())
                            .detail("device_name", entry.device_name.clone())
                            .detail("bulk", true),
                    )
                    .await;
            }
            Json(serde_json::json!({ "success": true, "revoked": revoked.len() })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to revoke other sessions");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not revoke the other sessions.",
            )
        }
    }
}

// ─── Admin oversight (AC-3) ──────────────────────────────────────────────────

/// `GET /admin/users/{user_id}/sessions` — any account's active sessions.
async fn admin_sessions_page(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<Uuid>,
) -> Response {
    if let Err(resp) = require_admin(&state, &session).await {
        return resp;
    }

    let Ok(Some(subject)) = state.users().find_by_id(user_id).await else {
        return render_not_found();
    };

    let entries = state
        .session_registry()
        .list(user_id)
        .await
        .unwrap_or_default();
    // An admin viewing someone else's list has no "current" session in it.
    let views: Vec<SessionView> = entries.iter().map(|e| to_view(e, None)).collect();

    let csrf_token = crate::form::csrf::generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("user", &subject);
    context.insert("sessions", &views);
    context.insert("is_admin_view", &true);
    crate::routes::helpers::inject_site_context(
        &state,
        &session,
        &mut context,
        &format!("/admin/users/{user_id}/sessions"),
    )
    .await;

    match state.theme().tera().render("user/sessions.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the admin sessions page");
            render_server_error("Could not render the sessions page.")
        }
    }
}

/// `POST /admin/users/{user_id}/sessions/{device_id}/revoke` — admin remote
/// logout of any account's session.
async fn admin_revoke_session(
    State(state): State<AppState>,
    session: Session,
    Path((user_id, device_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Response {
    let admin = match require_admin(&state, &session).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err((status, body)) = require_csrf_header(&session, &headers).await {
        return (status, body).into_response();
    }
    let ip = client_ip_of(&headers);

    match state.session_registry().revoke(user_id, device_id).await {
        Ok(Some(entry)) => {
            warn!(
                admin_id = %admin.id,
                user_id = %user_id,
                device = %device_id,
                "session revoked by admin"
            );
            state
                .security_audit()
                .emit(
                    SecurityEvent::new(SecurityEventKind::SessionRevokedByAdmin)
                        // The subject is the account acted upon; the actor is the
                        // admin. Both are recorded, because "who did this to my
                        // account?" is the question this event answers.
                        .user(user_id)
                        .actor(admin.id)
                        .subject(&entry.session_id)
                        .ip(ip)
                        .user_agent(entry.user_agent.clone())
                        .detail("device_id", device_id.to_string())
                        .detail("device_name", entry.device_name),
                )
                .await;
            Json(serde_json::json!({ "success": true })).into_response()
        }
        Ok(None) => json_error(StatusCode::NOT_FOUND, "No such session."),
        Err(e) => {
            tracing::error!(error = %e, "failed to revoke a session as admin");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not revoke the session.",
            )
        }
    }
}

/// The session-management router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/sessions", get(sessions_page))
        .route("/user/sessions/{device_id}/rename", post(rename_session))
        .route("/user/sessions/{device_id}/revoke", post(revoke_session))
        .route("/user/sessions/revoke-others", post(revoke_other_sessions))
        .route("/admin/users/{user_id}/sessions", get(admin_sessions_page))
        .route(
            "/admin/users/{user_id}/sessions/{device_id}/revoke",
            post(admin_revoke_session),
        )
}
