//! Enforce the two pending-security-state session flags (B304):
//! `SESSION_ENROLLMENT_REQUIRED` (set by `routes::auth::setup_session` when
//! the account must enrol a second factor) and
//! `SESSION_PASSWORD_CHANGE_REQUIRED` (set after a password login whose
//! password has aged out).
//!
//! While either flag is set, every request but a short allow-list is turned
//! back: a 303 to the right page for a browser, 403 for JSON and `/api`.
//! Enrolling, or changing the password, clears the flag that sent the visitor
//! here. A request authenticated by an API token or a bearer token never
//! carries either flag — such a request has no session these flags live in.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use tower_sessions::Session;

use crate::routes::auth::{SESSION_ENROLLMENT_REQUIRED, SESSION_PASSWORD_CHANGE_REQUIRED};
use crate::state::AppState;

/// Where the enrolment flag sends a browser, and what it exempts besides
/// itself: the passkey pages (registering one also satisfies the
/// requirement), logging out, and static assets.
const ENROLLMENT_TARGET: &str = "/user/two-factor";
const ENROLLMENT_EXEMPT_PREFIXES: &[&str] =
    &["/user/two-factor", "/user/webauthn", "/user/passkeys"];

/// Where the password-change flag sends a browser: the profile page, which
/// holds the change-password form, and the endpoint that submits it.
const PASSWORD_CHANGE_TARGET: &str = "/user/profile";
const PASSWORD_CHANGE_EXEMPT_PREFIXES: &[&str] = &["/user/profile", "/user/password"];

/// Exempt for both flags: logging out (never trap a visitor who wants to
/// leave) and static assets.
const ALWAYS_EXEMPT_PREFIXES: &[&str] = &["/user/logout", "/static", "/favicon.ico"];

fn under_any(path: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Whether the request prefers a JSON refusal over an HTML redirect: under
/// `/api`, or a JSON caller that cannot follow a redirect to a themed page.
fn wants_json(request: &Request<Body>) -> bool {
    if request.uri().path().starts_with("/api") {
        return true;
    }
    request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("application/json"))
        || request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("application/json"))
}

fn refusal(request: &Request<Body>, target: &'static str, reason: &'static str) -> Response {
    if wants_json(request) {
        crate::error::AppError::forbidden(reason).into_response()
    } else {
        (StatusCode::SEE_OTHER, Redirect::to(target)).into_response()
    }
}

/// The middleware. Sits after the session and authentication layers, so it
/// can read the session's flags and knows whether this request was token- or
/// bearer-authenticated (and therefore carries neither).
pub async fn enforce_pending_security_state(
    State(_state): State<AppState>,
    session: Session,
    request: Request<Body>,
    next: Next,
) -> Response {
    // A request authenticated by an API token or a bearer token has no
    // session of its own these flags could live in.
    if crate::middleware::api_token::is_token_authenticated()
        || request
            .extensions()
            .get::<crate::middleware::bearer_auth::BearerAuth>()
            .is_some()
    {
        return next.run(request).await;
    }

    let path = request.uri().path();
    if under_any(path, ALWAYS_EXEMPT_PREFIXES) {
        return next.run(request).await;
    }

    let enrollment_required: bool = session
        .get(SESSION_ENROLLMENT_REQUIRED)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if enrollment_required {
        if under_any(path, ENROLLMENT_EXEMPT_PREFIXES) {
            return next.run(request).await;
        }
        return refusal(
            &request,
            ENROLLMENT_TARGET,
            "A second factor is required on this account before it may be used further.",
        );
    }

    let password_change_required: bool = session
        .get(SESSION_PASSWORD_CHANGE_REQUIRED)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if password_change_required {
        if under_any(path, PASSWORD_CHANGE_EXEMPT_PREFIXES) {
            return next.run(request).await;
        }
        return refusal(
            &request,
            PASSWORD_CHANGE_TARGET,
            "This account's password has expired and must be changed before continuing.",
        );
    }

    next.run(request).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn prefix_matching_requires_a_path_boundary() {
        assert!(under_any("/user/two-factor", ENROLLMENT_EXEMPT_PREFIXES));
        assert!(under_any(
            "/user/two-factor/setup",
            ENROLLMENT_EXEMPT_PREFIXES
        ));
        // `/user/two-factor-evil` must not match: it is a different path that
        // happens to share a prefix, not a sub-path.
        assert!(!under_any(
            "/user/two-factor-evil",
            ENROLLMENT_EXEMPT_PREFIXES
        ));
        assert!(!under_any("/admin", ENROLLMENT_EXEMPT_PREFIXES));
        assert!(!under_any("/", ENROLLMENT_EXEMPT_PREFIXES));
    }

    #[test]
    fn password_change_exempts_profile_and_password_only() {
        assert!(under_any("/user/profile", PASSWORD_CHANGE_EXEMPT_PREFIXES));
        assert!(under_any("/user/password", PASSWORD_CHANGE_EXEMPT_PREFIXES));
        assert!(!under_any(
            "/user/two-factor",
            PASSWORD_CHANGE_EXEMPT_PREFIXES
        ));
        assert!(!under_any("/", PASSWORD_CHANGE_EXEMPT_PREFIXES));
    }
}
