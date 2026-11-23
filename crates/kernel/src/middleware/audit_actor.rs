//! Who is acting, for the audit trail.
//!
//! Write services record who made a change and from where without taking a
//! request: this layer reads the signed-in user (a session or an API token)
//! and the vetted client address once, and runs the rest of the request with
//! them as its [`AuditActor`]. Inside the session and API token layers, so
//! both have run.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use tower_sessions::Session;
use uuid::Uuid;

use crate::routes::auth::SESSION_USER_ID;
use crate::services::audit::AuditActor;

/// Run the request with its actor in scope.
pub async fn scope_audit_actor(session: Session, req: Request, next: Next) -> Response {
    let token_user = req
        .extensions()
        .get::<crate::middleware::api_token::ApiTokenAuth>()
        .map(|t| t.user_id);
    let user_id = match token_user {
        Some(id) => Some(id),
        None => session.get::<Uuid>(SESSION_USER_ID).await.ok().flatten(),
    };
    let actor = AuditActor {
        user_id,
        ip: req
            .extensions()
            .get::<crate::middleware::ClientIp>()
            .map(|c| c.0.clone())
            .unwrap_or_default(),
        user_agent: req
            .headers()
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    };
    actor.scope(next.run(req)).await
}
