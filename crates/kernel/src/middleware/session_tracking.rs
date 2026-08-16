//! Per-request maintenance of the multi-device session index (FR-7b, D-36).
//!
//! One middleware owns every write to the index: first registration, the
//! `cycle_id` migration, the throttled `last_seen` touch, and the enforcement of
//! revocation. Concentrating them here is what keeps them consistent — there is
//! no second place that could write a different view of the same session.
//!
//! # Why it runs *after* the handler
//!
//! The work happens after `next.run(req)` because the interesting session state
//! is what the handler *left behind*: a login has just written
//! `SESSION_USER_ID`, and a `cycle_id` has just fired. Observing before the
//! handler would miss both by exactly one request.
//!
//! # Revocation enforcement
//!
//! A session that carries [`SESSION_REGISTERED`] but has no index entry was
//! revoked, so this middleware deletes the session. That makes "a revoked
//! session's next request fails" a property the kernel enforces, rather than one
//! that depends on the store deletion having raced correctly against a
//! `cycle_id` (see [`crate::services::session_registry`] for why that race
//! exists at all).

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use tower_sessions::Session;
use tracing::warn;
use uuid::Uuid;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::routes::auth::SESSION_USER_ID;
use crate::services::session_registry::{Observation, SESSION_DEVICE_ID, SESSION_REGISTERED};
use crate::state::AppState;

/// Keep the session index current for the authenticated caller.
pub async fn track_session(
    State(state): State<AppState>,
    session: Session,
    req: Request,
    next: Next,
) -> Response {
    // Capture request-scoped context before the body is consumed.
    let ip = req
        .extensions()
        .get::<crate::middleware::ClientIp>()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    let user_agent = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let response = next.run(req).await;

    // Anonymous traffic is the overwhelming majority and touches nothing here.
    let Some(user_id) = session.get::<Uuid>(SESSION_USER_ID).await.ok().flatten() else {
        return response;
    };

    // Immediately after `cycle_id` the session has no id until the layer saves
    // it — which happens outside this middleware. Skip that one request; the
    // next one carries the new id and the entry migrates then.
    let Some(session_id) = session.id().map(|id| id.to_string()) else {
        return response;
    };

    // The device id lives in the session record, so it survives `cycle_id`.
    let device_id = match session.get::<Uuid>(SESSION_DEVICE_ID).await.ok().flatten() {
        Some(id) => id,
        None => {
            let id = Uuid::now_v7();
            if let Err(e) = session.insert(SESSION_DEVICE_ID, id).await {
                warn!(error = %e, "failed to assign a device id to the session");
                return response;
            }
            id
        }
    };

    let was_registered = session
        .get::<bool>(SESSION_REGISTERED)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);

    let now = chrono::Utc::now().timestamp();
    let observation = match state
        .session_registry()
        .observe(
            user_id,
            device_id,
            &session_id,
            &ip,
            &user_agent,
            was_registered,
            now,
        )
        .await
    {
        Ok(o) => o,
        Err(e) => {
            // A Redis hiccup must not break the request. The consequence is a
            // stale device list, not a failed page.
            warn!(error = %e, "failed to update the session index");
            return response;
        }
    };

    match observation {
        Observation::Registered => {
            if let Err(e) = session.insert(SESSION_REGISTERED, true).await {
                warn!(error = %e, "failed to mark the session registered");
            }
            state
                .security_audit()
                .emit(
                    SecurityEvent::new(SecurityEventKind::SessionCreated)
                        .user(user_id)
                        // Hashed, never the raw id (the D-36 rider).
                        .subject(&session_id)
                        .ip(ip)
                        .user_agent(user_agent)
                        .detail("device_id", device_id.to_string()),
                )
                .await;
        }
        Observation::Cycled => {
            state
                .security_audit()
                .emit(
                    SecurityEvent::new(SecurityEventKind::SessionIdCycled)
                        .user(user_id)
                        // The hash of the NEW id, so the trail follows forward.
                        .subject(&session_id)
                        .ip(ip)
                        .user_agent(user_agent)
                        .detail("device_id", device_id.to_string()),
                )
                .await;
        }
        Observation::Seen => {}
        Observation::Revoked => {
            // The session was in the index and is not any more: revoked from
            // another device or by an admin. Terminate it here so the next
            // request is anonymous regardless of what the store still holds.
            if let Err(e) = session.delete().await {
                warn!(error = %e, "failed to delete a revoked session");
            }
        }
    }

    response
}
