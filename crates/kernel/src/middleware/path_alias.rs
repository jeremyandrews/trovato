//! Path alias middleware for URL rewriting.
//!
//! Rewrites incoming requests from alias paths to their source paths,
//! enabling human-readable URLs like /about-us instead of /item/{uuid}.
//!
//! Stage-aware: reads `active_stage` from the session. Tries the active
//! stage first, then falls back to `"live"`.

use axum::{
    body::Body,
    extract::State,
    http::{Request, Uri},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tower_sessions::Session;

use uuid::Uuid;

use crate::middleware::language::ResolvedLanguage;
use crate::models::UrlAlias;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::auth::SESSION_ACTIVE_STAGE;
use crate::state::AppState;

/// Middleware to resolve path aliases to their source paths.
///
/// If the incoming request path matches a URL alias, the request URI
/// is rewritten to the source path (internal rewrite, no redirect).
/// This allows the router to handle the request as if it came to the
/// original source path.
///
/// Stage-aware: reads `active_stage` from the user session. Tries the
/// active stage alias first; if not found, falls back to `"live"`.
/// Anonymous users always resolve against `"live"`.
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
    session: Session,
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

    // Extract resolved language from request extensions (set by negotiate_language middleware)
    let language = request
        .extensions()
        .get::<ResolvedLanguage>()
        .map(|l| l.0.as_str())
        .unwrap_or_else(|| state.default_language());

    // Read active stage from session (default live for anonymous users)
    let active_stage: Uuid = match session.get::<String>(SESSION_ACTIVE_STAGE).await {
        Ok(Some(s)) => match s.parse::<Uuid>() {
            Ok(uuid) => uuid,
            Err(_) => {
                tracing::warn!(
                    raw_value = %s,
                    "session active_stage is not a valid UUID, defaulting to live"
                );
                LIVE_STAGE_ID
            }
        },
        _ => LIVE_STAGE_ID,
    };

    // Look up alias in database with language and stage context.
    // Try the active stage first; fall back to live if not found.
    tracing::debug!(path = %path, language = %language, stage = %active_stage, "looking up path alias");

    let alias_result = lookup_alias(state.db(), path, active_stage, language).await;

    if let Some(alias) = alias_result {
        tracing::debug!(
            alias = %alias.alias,
            source = %alias.source,
            stage = %alias.stage_id,
            "found path alias, rewriting"
        );
        if let Ok(new_uri) = rewrite_uri(request.uri(), &alias.source) {
            tracing::debug!(new_uri = %new_uri, "URI rewritten");
            *request.uri_mut() = new_uri;
        }
    } else {
        tracing::debug!(path = %path, "no alias found for path");
    }

    next.run(request).await
}

/// Look up an alias, trying the active stage first then falling back to live.
async fn lookup_alias(
    pool: &sqlx::PgPool,
    path: &str,
    stage_id: Uuid,
    language: &str,
) -> Option<UrlAlias> {
    // Try active stage first
    match UrlAlias::find_by_alias_with_context(pool, path, stage_id, language).await {
        Ok(Some(alias)) => return Some(alias),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(path = %path, stage = %stage_id, error = %e, "error looking up alias");
        }
    }

    // Fall back to live if we were looking in a different stage
    if stage_id != LIVE_STAGE_ID {
        match UrlAlias::find_by_alias_with_context(pool, path, LIVE_STAGE_ID, language).await {
            Ok(Some(alias)) => return Some(alias),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "error looking up live alias fallback");
            }
        }
    }

    None
}

/// Rewrite a URI to a new path while preserving query string.
fn rewrite_uri(original: &Uri, new_path: &str) -> Result<Uri, axum::http::uri::InvalidUri> {
    // Preserve query string if present
    if let Some(query) = original.query() {
        format!("{new_path}?{query}").parse()
    } else {
        new_path.parse()
    }
}

/// Fallback handler for path alias resolution.
///
/// Called for requests that don't match any registered route. Looks up URL
/// aliases in the database and, if found, forwards the request to the inner
/// router with the rewritten URI (transparent internal rewrite).
///
/// In Axum 0.8, `Router::layer()` middleware runs after route matching, so
/// URI rewriting in middleware cannot affect which route is matched. This
/// fallback receives all unmatched requests and re-dispatches them to the
/// inner router after resolving any URL alias.
pub async fn path_alias_fallback(
    state: AppState,
    session: Session,
    router: std::sync::Arc<axum::Router>,
    mut request: Request<Body>,
) -> Response {
    use crate::models::stage::LIVE_STAGE_ID;
    use crate::routes::auth::SESSION_ACTIVE_STAGE;
    use tower::ServiceExt;

    let raw_path = request.uri().path();

    // Redirect trailing-slash URLs to their canonical no-slash form so that
    // `/conferences/` resolves the same alias as `/conferences`.  The root `/`
    // is the only path that is allowed to keep its trailing slash.
    if raw_path.len() > 1 && raw_path.ends_with('/') {
        let canonical = raw_path.trim_end_matches('/');
        let location = if let Some(query) = request.uri().query() {
            format!("{canonical}?{query}")
        } else {
            canonical.to_string()
        };
        return axum::response::Redirect::permanent(&location).into_response();
    }

    let path = raw_path.to_string();

    // Read resolved language from request extensions
    let language = request
        .extensions()
        .get::<ResolvedLanguage>()
        .map(|l| l.0.clone())
        .unwrap_or_else(|| state.default_language().to_string());

    // Read active stage from session (default to live)
    let active_stage: Uuid = match session.get::<String>(SESSION_ACTIVE_STAGE).await {
        Ok(Some(s)) => s.parse::<Uuid>().unwrap_or(LIVE_STAGE_ID),
        _ => LIVE_STAGE_ID,
    };

    // Look up alias: try active stage, then fall back to live
    let alias = lookup_alias(state.db(), &path, active_stage, &language).await;

    if let Some(alias) = alias {
        tracing::debug!(
            alias = %alias.alias,
            source = %alias.source,
            "path alias fallback: rewriting and forwarding"
        );
        // Rewrite the URI
        if let Ok(new_uri) = rewrite_uri(request.uri(), &alias.source) {
            *request.uri_mut() = new_uri;
            // Forward to the inner router with the rewritten URI
            return router
                .as_ref()
                .clone()
                .oneshot(request)
                .await
                .unwrap_or_else(|err| match err {});
        }
    }

    // No alias found — return 404
    axum::http::StatusCode::NOT_FOUND.into_response()
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
