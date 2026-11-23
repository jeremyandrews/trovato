//! API token authentication middleware.
//!
//! Checks for `Authorization: Bearer <token>` headers and, if valid,
//! authenticates the request as the token's user **for this request only**.
//!
//! # Stateless by construction
//!
//! A token is a credential the client sends on every request, so a token
//! request must leave nothing behind: no cookie, no server-side session, no
//! entry in the per-user session index. Handlers, though, find the current user
//! by reading `SESSION_USER_ID` from the request's `Session`, and there are
//! dozens of those readers. Rather than teach each of them about tokens, the
//! middleware replaces the request's `Session` extension with an *ephemeral*
//! session backed by a throwaway [`MemoryStore`] that holds the token's user.
//! Every existing reader works unchanged, and nothing it writes goes anywhere.
//!
//! This relies on how `tower-sessions` (0.14) saves: `SessionManager` keeps its
//! own clone of the session it created, inserts another clone into the request
//! extensions, and after the inner service returns it inspects and saves only
//! its own clone. Replacing the extension therefore cannot cause a save, a
//! `Set-Cookie` or a Redis record: the manager's session is never touched, and
//! the ephemeral one is never saved by anyone.

use crate::error::AppError;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use tower_sessions::{MemoryStore, Session};
use uuid::Uuid;

use crate::middleware::bearer_auth::BearerAuth;
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

/// Middleware that authenticates via Bearer token.
///
/// If an `Authorization: Bearer <token>` header is present:
/// - Valid token (decided by [`crate::services::api_token::resolve`], the one
///   resolver the MCP server shares) -> the rest of this request sees an
///   ephemeral session holding the token's user (see the module docs), and
///   [`ApiTokenAuth`] is inserted
/// - Invalid/expired, or the token's user is blocked -> returns 401 JSON error
/// - No header -> passes through (session auth may still work)
///
/// If the session already contains a user_id (cookie auth), that takes precedence
/// and the Bearer token is ignored. This avoids overwriting an existing cookie
/// session with a potentially different token user.
pub async fn authenticate_api_token(
    State(state): State<AppState>,
    session: Session,
    request: Request<Body>,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let raw_token = match auth_header {
        Some(v) if v.starts_with("Bearer ") => &v[7..],
        _ => return next.run(request).await,
    };

    // If bearer auth middleware already authenticated this request via JWT,
    // skip API token lookup to avoid rejecting valid JWT tokens.
    if request.extensions().get::<BearerAuth>().is_some() {
        return next.run(request).await;
    }

    // If session already has a user (cookie auth), let it take precedence.
    if let Ok(Some(_)) = session.get::<Uuid>(SESSION_USER_ID).await {
        return next.run(request).await;
    }

    let token = match crate::services::api_token::resolve(&state, raw_token).await {
        Ok(Some(holder)) => holder,
        Ok(None) => return invalid_token(),
        Err(e) => {
            tracing::error!(error = %e, "failed to look up API token");
            return AppError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error",
            )
            .into_response();
        }
    };
    let user_id = token.user.id;

    // An ephemeral session for the rest of this request, holding the token's
    // user, in place of the cookie session the session layer created. The
    // layer's own session object is left untouched, so it is not saved and no
    // cookie is sent.
    let ephemeral = Session::new(None, Arc::new(MemoryStore::default()), None);
    if let Err(e) = ephemeral.insert(SESSION_USER_ID, user_id).await {
        tracing::error!(error = %e, "failed to build the request session for an API token");
        return AppError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            .into_response();
    }

    // Mark the request as bearer-authenticated so a downstream handler can tell
    // "this caller presented a token" from "this caller had a session cookie".
    // The two differ for CSRF: a cookie is *ambient* credentials a browser
    // attaches to a cross-site request by itself, which is the whole attack CSRF
    // tokens exist to stop; a bearer token is not attached by any browser
    // automatically, so a request authenticated by one carries no CSRF exposure.
    // See `routes::plugin_api` (G-CSRF-NO-BEARER-BYPASS).
    let mut request = request;
    request.extensions_mut().insert(ephemeral);
    request.extensions_mut().insert(ApiTokenAuth { user_id });

    // The same fact, for code that only has the session and the headers to go
    // on (`routes::helpers::require_csrf_header`). A task-local rather than a
    // session key: it exists only while this request's future runs, only this
    // middleware can set it, and no cookie session can ever carry it.
    TOKEN_AUTHENTICATED.scope((), next.run(request)).await
}

/// The 401 for a token that does not authenticate anyone: unknown, expired,
/// revoked, or belonging to a blocked account.
fn invalid_token() -> Response {
    AppError::unauthorized("Invalid or expired API token").into_response()
}

tokio::task_local! {
    /// Set for the duration of a request this middleware authenticated.
    static TOKEN_AUTHENTICATED: ();
}

/// Whether the request currently being handled was authenticated by an API
/// token (and not by a session cookie).
///
/// True only inside a request that [`authenticate_api_token`] authenticated.
/// Code that spawns a task loses it, so the answer there is `false`, which is
/// the safe direction: a CSRF check falls back to requiring a token.
pub fn is_token_authenticated() -> bool {
    TOKEN_AUTHENTICATED.try_with(|_| ()).is_ok()
}

/// Marker inserted when an `Authorization: Bearer <api-token>` **authenticated**
/// this request.
///
/// Deliberately not inserted when a session cookie already identified the user:
/// in that case the bearer header was ignored and the request is cookie-
/// authenticated, so it is CSRF-exposed exactly as before.
#[derive(Debug, Clone, Copy)]
pub struct ApiTokenAuth {
    /// The user the token authenticated.
    pub user_id: Uuid,
}
