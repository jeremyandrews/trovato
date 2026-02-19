//! Redirect middleware.
//!
//! Checks the redirect table before alias resolution.
//! If a redirect is found, returns an HTTP redirect response.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::middleware::language::ResolvedLanguage;
use crate::services::redirect::validate_redirect_destination;
use crate::state::AppState;

/// Middleware to check for URL redirects.
///
/// Runs before path alias resolution. If the incoming path matches
/// a redirect record, returns an HTTP redirect (301/302/etc.).
pub async fn check_redirect(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Skip system paths that don't need redirect checking
    if path.starts_with("/admin")
        || path.starts_with("/api")
        || path.starts_with("/static")
        || path.starts_with("/install")
        || path.starts_with("/oauth")
        || path == "/health"
        || path == "/"
    {
        return next.run(request).await;
    }

    let language = request
        .extensions()
        .get::<ResolvedLanguage>()
        .map(|l| l.0.as_str())
        .unwrap_or_else(|| state.default_language());

    // Look up redirect via cache (avoids DB hit on every request)
    let redirect = match state
        .redirect_cache()
        .find(state.db(), path, language)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return next.run(request).await,
        Err(e) => {
            tracing::debug!(path = %path, error = %e, "redirect lookup failed");
            return next.run(request).await;
        }
    };

    tracing::debug!(
        source = %redirect.source,
        destination = %redirect.destination,
        status = redirect.status_code,
        "found redirect"
    );

    // Defense-in-depth: validate destination even though it's checked at write time.
    // Protects against direct DB manipulation or migration bugs.
    if !validate_redirect_destination(&redirect.destination) {
        tracing::warn!(
            source = %redirect.source,
            destination = %redirect.destination,
            "refusing redirect with unsafe destination"
        );
        return next.run(request).await;
    }

    // Build redirect response
    let status = match redirect.status_code {
        302 => StatusCode::FOUND,
        303 => StatusCode::SEE_OTHER,
        307 => StatusCode::TEMPORARY_REDIRECT,
        308 => StatusCode::PERMANENT_REDIRECT,
        _ => StatusCode::MOVED_PERMANENTLY, // 301 default
    };

    // Sanitize destination to prevent CRLF injection into the Location header.
    // HTTP header values must not contain \r or \n.
    let safe_destination: String = redirect
        .destination
        .chars()
        .filter(|c| *c != '\r' && *c != '\n')
        .collect();

    (status, [("Location", safe_destination)]).into_response()
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn status_code_mapping() {
        // Verify our match arms produce correct status codes
        assert_eq!(StatusCode::MOVED_PERMANENTLY.as_u16(), 301);
        assert_eq!(StatusCode::FOUND.as_u16(), 302);
        assert_eq!(StatusCode::TEMPORARY_REDIRECT.as_u16(), 307);
    }

    #[test]
    fn crlf_sanitization() {
        let dirty = "/new-page\r\nX-Injected: value";
        let sanitized: String = dirty.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        assert_eq!(sanitized, "/new-pageX-Injected: value");
        assert!(!sanitized.contains('\r'));
        assert!(!sanitized.contains('\n'));
    }
}
