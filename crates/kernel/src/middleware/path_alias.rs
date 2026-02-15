//! Path alias middleware for URL rewriting.
//!
//! Rewrites incoming requests from alias paths to their source paths,
//! enabling human-readable URLs like /about-us instead of /item/{uuid}.

use axum::{
    body::Body,
    extract::State,
    http::{Request, Uri},
    middleware::Next,
    response::Response,
};

use crate::models::UrlAlias;
use crate::state::AppState;

/// Middleware to resolve path aliases to their source paths.
///
/// If the incoming request path matches a URL alias, the request URI
/// is rewritten to the source path (internal rewrite, no redirect).
/// This allows the router to handle the request as if it came to the
/// original source path.
///
/// System paths are skipped to avoid unnecessary database lookups:
/// - /admin/* - Admin interface
/// - /api/* - API endpoints
/// - /static/* - Static files
/// - /install/* - Installer
/// - /user/* - User authentication
/// - /health - Health check
/// - /item/* - Direct item access (source paths)
pub async fn resolve_path_alias(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Skip system paths that don't need alias resolution
    if path.starts_with("/admin")
        || path.starts_with("/api")
        || path.starts_with("/static")
        || path.starts_with("/install")
        || path.starts_with("/user")
        || path.starts_with("/item")
        || path == "/health"
        || path == "/"
    {
        return next.run(request).await;
    }

    // Look up alias in database
    tracing::debug!(path = %path, "looking up path alias");
    match UrlAlias::find_by_alias(state.db(), path).await {
        Ok(Some(alias)) => {
            tracing::debug!(
                alias = %alias.alias,
                source = %alias.source,
                "found path alias, rewriting"
            );
            // Rewrite URI to source path
            if let Ok(new_uri) = rewrite_uri(request.uri(), &alias.source) {
                tracing::debug!(new_uri = %new_uri, "URI rewritten");
                *request.uri_mut() = new_uri;
            }
        }
        Ok(None) => {
            tracing::debug!(path = %path, "no alias found for path");
        }
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "error looking up alias");
        }
    }

    next.run(request).await
}

/// Rewrite a URI to a new path while preserving query string.
fn rewrite_uri(original: &Uri, new_path: &str) -> Result<Uri, axum::http::uri::InvalidUri> {
    // Preserve query string if present
    if let Some(query) = original.query() {
        format!("{}?{}", new_path, query).parse()
    } else {
        new_path.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_uri_simple() {
        let original: Uri = "/about-us".parse().unwrap();
        let result = rewrite_uri(&original, "/item/123").unwrap();
        assert_eq!(result.path(), "/item/123");
        assert_eq!(result.query(), None);
    }

    #[test]
    fn test_rewrite_uri_with_query() {
        let original: Uri = "/about-us?foo=bar".parse().unwrap();
        let result = rewrite_uri(&original, "/item/123").unwrap();
        assert_eq!(result.path(), "/item/123");
        assert_eq!(result.query(), Some("foo=bar"));
    }
}
