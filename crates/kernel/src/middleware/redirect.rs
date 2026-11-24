//! Redirect middleware.
//!
//! Checks the redirect table before alias resolution.
//! If a redirect is found, returns an HTTP redirect response.
//!
//! Two things an old link needs (B160):
//!
//! - **A redirect keeps the query string.** `/old?page=2` redirected to `/new`
//!   lost its `?page=2`; it now lands on `/new?page=2`.
//! - **Drupal's non-clean URLs work.** A Drupal site without clean URLs
//!   published every link as `/?q=node/96`, which reached Trovato's front page.
//!   A request for the site root with a `q` that names a path is that path:
//!   it follows that path's redirect when there is one, and otherwise is sent
//!   to the path itself, with any other query parameters kept.

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

/// The path a Drupal non-clean URL names (`/?q=node/96` → `/node/96`), and the
/// rest of its query string, for a request at the site root.
///
/// `None` when there is no `q`, or it is empty, or it is not a plain site path:
/// anything that could leave the site (`//host`, a scheme, a backslash, a
/// control character) is refused rather than redirected to.
pub(crate) fn drupal_q_path(query: Option<&str>) -> Option<(String, Option<String>)> {
    let query = query?;
    let mut q: Option<String> = None;
    let mut rest: Vec<&str> = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "q" && q.is_none() {
            let value = value.replace('+', " ");
            q = Some(urlencoding::decode(&value).ok()?.into_owned());
        } else {
            rest.push(pair);
        }
    }
    let q = q?;
    let q = q.trim();
    if q.starts_with("//") {
        return None;
    }
    let q = q.trim_start_matches('/');
    if q.is_empty() || q.contains("://") || q.contains('\\') || q.chars().any(char::is_control) {
        return None;
    }
    let path = format!("/{}", q.split(['?', '#']).next().unwrap_or(q));
    let rest = (!rest.is_empty()).then(|| rest.join("&"));
    Some((path, rest))
}

/// `destination` with the request's query string kept: appended with `?`, or
/// with `&` when the destination carries a query of its own, before any
/// fragment.
pub(crate) fn with_query(destination: &str, query: Option<&str>) -> String {
    let Some(query) = query.filter(|q| !q.is_empty()) else {
        return destination.to_string();
    };
    let (base, fragment) = match destination.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (destination, None),
    };
    let joiner = if base.contains('?') { '&' } else { '?' };
    match fragment {
        Some(fragment) => format!("{base}{joiner}{query}#{fragment}"),
        None => format!("{base}{joiner}{query}"),
    }
}

/// A redirect response to `location`, CR and LF removed so nothing can be
/// injected into the `Location` header.
fn redirect_to(status: StatusCode, location: &str) -> Response {
    let safe: String = location
        .chars()
        .filter(|c| *c != '\r' && *c != '\n')
        .collect();
    (status, [("Location", safe)]).into_response()
}

/// The status a stored redirect answers with.
fn status_of(code: i16) -> StatusCode {
    match code {
        302 => StatusCode::FOUND,
        303 => StatusCode::SEE_OTHER,
        307 => StatusCode::TEMPORARY_REDIRECT,
        308 => StatusCode::PERMANENT_REDIRECT,
        _ => StatusCode::MOVED_PERMANENTLY, // 301 default
    }
}

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
    let query = request.uri().query();

    let language = request
        .extensions()
        .get::<ResolvedLanguage>()
        .map(|l| l.0.as_str())
        .unwrap_or_else(|| state.default_language());

    // A Drupal non-clean URL: the site root (as requested, before a configured
    // front page replaced it) with `?q=` naming a path.
    let at_root = path == "/"
        || path == "/index.php"
        || request
            .extensions()
            .get::<crate::middleware::RequestedPath>()
            .is_some_and(|requested| requested.0 == "/" || requested.0 == "/index.php");
    if at_root && let Some((old_path, rest)) = drupal_q_path(query) {
        // The path's own redirect, in one hop, when there is one; otherwise
        // the path itself, where routing and aliases take over.
        if let Some(cache) = state.redirect_cache()
            && let Ok(Some(redirect)) = cache.find(state.db(), &old_path, language).await
            && validate_redirect_destination(&redirect.destination)
        {
            return redirect_to(
                status_of(redirect.status_code),
                &with_query(&redirect.destination, rest.as_deref()),
            );
        }
        return redirect_to(
            StatusCode::MOVED_PERMANENTLY,
            &with_query(&old_path, rest.as_deref()),
        );
    }

    // Skip system paths that don't need redirect checking
    if path.starts_with("/admin")
        || path.starts_with("/api")
        || path.starts_with("/static")
        || path.starts_with("/files")
        || path.starts_with("/file")
        || path.starts_with("/install")
        || path.starts_with("/oauth")
        || crate::routes::health::is_probe_path(path)
        || path == "/"
    {
        return next.run(request).await;
    }

    // Skip when the redirects plugin is disabled
    let Some(cache) = state.redirect_cache() else {
        return next.run(request).await;
    };

    // Look up redirect via cache (avoids DB hit on every request)
    let redirect = match cache.find(state.db(), path, language).await {
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

    // The request's query string goes with it (B160).
    redirect_to(
        status_of(redirect.status_code),
        &with_query(&redirect.destination, query),
    )
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn status_code_mapping() {
        assert_eq!(status_of(301), StatusCode::MOVED_PERMANENTLY);
        assert_eq!(status_of(302), StatusCode::FOUND);
        assert_eq!(status_of(307), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(status_of(0), StatusCode::MOVED_PERMANENTLY);
    }

    #[test]
    fn crlf_sanitization() {
        let response = redirect_to(StatusCode::FOUND, "/new-page\r\nX-Injected: value");
        let location = response
            .headers()
            .get("Location")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(location, "/new-pageX-Injected: value");
    }

    #[test]
    fn a_redirect_keeps_the_query_string() {
        assert_eq!(with_query("/new", Some("page=2")), "/new?page=2");
        assert_eq!(with_query("/new?a=1", Some("page=2")), "/new?a=1&page=2");
        assert_eq!(with_query("/new#top", Some("page=2")), "/new?page=2#top");
        assert_eq!(with_query("/new", None), "/new");
        assert_eq!(with_query("/new", Some("")), "/new");
    }

    #[test]
    fn a_drupal_q_url_names_its_path() {
        assert_eq!(
            drupal_q_path(Some("q=node/96")),
            Some(("/node/96".to_string(), None))
        );
        assert_eq!(
            drupal_q_path(Some("q=node%2F96&page=1")),
            Some(("/node/96".to_string(), Some("page=1".to_string())))
        );
        assert_eq!(
            drupal_q_path(Some("page=1&q=taxonomy/term/3")),
            Some(("/taxonomy/term/3".to_string(), Some("page=1".to_string())))
        );
        assert_eq!(drupal_q_path(Some("page=1")), None);
        assert_eq!(drupal_q_path(Some("q=")), None);
        assert_eq!(drupal_q_path(None), None);
    }

    #[test]
    fn a_q_that_could_leave_the_site_is_refused() {
        for q in [
            "q=//evil.example",
            "q=https://evil.example",
            "q=%2F%2Fevil.example",
            "q=/%5Cevil.example",
            "q=%5C%5Cevil.example",
            "q=node%0D%0ASet-Cookie:x",
        ] {
            assert_eq!(drupal_q_path(Some(q)), None, "{q}");
        }
    }
}
