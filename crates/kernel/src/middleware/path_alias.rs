//! Path alias middleware for URL rewriting.
//!
//! Rewrites incoming requests from alias paths to their source paths,
//! enabling human-readable URLs like /about-us instead of /item/{uuid}.
//!
//! Stage-aware for a holder of `use stages`: the session's active stage is
//! tried first, then Live. Everyone else is looked up on Live only
//! (`routes::helpers::read_stages`).

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
use crate::models::url_alias::AliasMatch;
use crate::state::AppState;

/// The stage an alias is looked up on first, before Live: the session's
/// active stage for a holder of `use stages`, else Live
/// ([`crate::routes::helpers::read_stages`]). The viewer is loaded only when the
/// session names a stage, so a request with none costs nothing extra.
async fn alias_stage(state: &AppState, session: &Session) -> Uuid {
    let Some(requested) = crate::routes::helpers::session_stage(session).await else {
        return LIVE_STAGE_ID;
    };
    let viewer = crate::routes::helpers::get_user_context(session, state).await;
    crate::routes::helpers::read_stages(state, &viewer, Some(requested))
        .await
        .first()
        .copied()
        .unwrap_or(LIVE_STAGE_ID)
}

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
        || path.starts_with("/files")
        || path.starts_with("/file")
        || path.starts_with("/install")
        || path.starts_with("/user")
        || path.starts_with("/item")
        || crate::routes::health::is_probe_path(path)
        || path == "/"
    {
        return next.run(request).await;
    }

    // Extract resolved language from request extensions (set by negotiate_language middleware)
    let default_language = state.default_language();
    let language = request
        .extensions()
        .get::<ResolvedLanguage>()
        .map(|l| l.0.as_str())
        .unwrap_or(&default_language);

    let active_stage = alias_stage(&state, &session).await;

    // Look up alias in database with language and stage context.
    // Try the active stage first; fall back to live if not found.
    tracing::debug!(path = %path, language = %language, stage = %active_stage, "looking up path alias");

    let alias_result = lookup_alias(state.db(), path, active_stage, language)
        .await
        .and_then(|found| match decide(found, None) {
            Resolution::Serve(alias) => Some(alias),
            Resolution::Redirect(_) => None,
        });

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

/// What the alias lookup decided for a requested path.
enum Resolution {
    /// Serve the alias's source at the requested address.
    Serve(UrlAlias),
    /// Send the visitor to this alias (a permanent redirect): the stored
    /// spelling of an alias asked for in another letter case, or the
    /// requested language's own alias of the same page.
    Redirect(String),
}

/// What to do with a match. `prefix_language` is the language the address
/// named by its prefix, if it had one.
///
/// - An exact match in that language, or for a page with no alias of its own
///   in it, is served: every page can be read under every prefix, and links on
///   a page in that language are written that way.
/// - An exact match of another language's alias, for a page that has its own
///   alias in the prefix's language, is redirected there (`/es/recipes/borscht-
///   with-pork-ribs` to `/es/recipes/borscht-con-costillas-de-cerdo`): the old
///   link works and the page keeps one address per language.
/// - A match only ignoring case is redirected to the stored spelling, so
///   `/BLOG/TILLICUM` reaches `/blog/tillicum` and nothing is served twice.
fn decide(found: AliasMatch, prefix_language: Option<&str>) -> Resolution {
    let own = found
        .own_alias
        .filter(|own| prefix_language.is_some() && *own != found.alias.alias);
    if !found.exact {
        return Resolution::Redirect(own.unwrap_or(found.alias.alias));
    }
    match (prefix_language, own) {
        (Some(language), Some(own)) if found.alias.language != language => {
            Resolution::Redirect(own)
        }
        _ => Resolution::Serve(found.alias),
    }
}

/// The forms `path` may be stored in: as requested, and percent-decoded when
/// that differs (and is UTF-8). The decoded form is last, which is the form
/// the case-insensitive match compares.
fn candidates(path: &str) -> Vec<String> {
    let mut forms = vec![path.to_string()];
    if let Ok(decoded) = urlencoding::decode(path)
        && decoded != path
    {
        forms.push(decoded.into_owned());
    }
    forms
}

/// `path` fit for a `Location` header: every byte that is not printable ASCII
/// (an alias with `á` or a space in it) percent-encoded, the rest as stored.
fn location_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_graphic() && byte != b'?' && byte != b'#' {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Look up an alias in one query, trying the active stage first then
/// falling back to live.
async fn lookup_alias(
    pool: &sqlx::PgPool,
    path: &str,
    stage_id: Uuid,
    language: &str,
) -> Option<AliasMatch> {
    let forms = candidates(path);
    let forms: Vec<&str> = forms.iter().map(String::as_str).collect();
    // Try active stage first
    match UrlAlias::resolve_path(pool, &forms, stage_id, language).await {
        Ok(Some(found)) => return Some(found),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(path = %path, stage = %stage_id, error = %e, "error looking up alias");
        }
    }

    // Fall back to live if we were looking in a different stage
    if stage_id != LIVE_STAGE_ID {
        match UrlAlias::resolve_path(pool, &forms, LIVE_STAGE_ID, language).await {
            Ok(Some(found)) => return Some(found),
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
    use tower::ServiceExt;

    let raw_path = request.uri().path().to_string();

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

    // Strip language prefix if present (e.g., /it/conferences → /conferences).
    // The language middleware sets ResolvedLanguage in extensions but can't
    // rewrite the URI for route matching in Axum 0.8 (middleware runs after
    // route matching). The fallback handles this by stripping the prefix
    // before looking up aliases.
    let known_languages = &state.known_languages();
    let default_language = &state.default_language();
    let (path, resolved_lang) = strip_language_prefix(&raw_path, known_languages);
    let had_lang_prefix = resolved_lang.is_some();

    // Read resolved language — prefer what we just stripped, fall back to
    // what the middleware set, then default.
    let language = resolved_lang
        .or_else(|| {
            request
                .extensions()
                .get::<ResolvedLanguage>()
                .map(|l| l.0.clone())
        })
        .unwrap_or_else(|| default_language.to_string());

    // Store the resolved language in extensions so downstream handlers see it.
    if let Some(ref lang) = Some(language.clone()) {
        request
            .extensions_mut()
            .insert(ResolvedLanguage(lang.clone()));
    }

    let active_stage = alias_stage(&state, &session).await;

    // The language prefix the address was asked with, if any: the one this
    // function stripped, or the one `negotiate_language` stripped before it
    // (the requested path ends with what is left).
    let asked_prefix: String = if had_lang_prefix {
        raw_path[..raw_path.len() - path.len()].to_string()
    } else {
        request
            .extensions()
            .get::<crate::middleware::RequestedPath>()
            .and_then(|asked| asked.0.strip_suffix(raw_path.as_str()).map(str::to_string))
            .filter(|prefix| prefix.len() > 1)
            .unwrap_or_default()
    };
    let prefix_language = asked_prefix
        .strip_prefix('/')
        .filter(|l| known_languages.iter().any(|k| k == l));
    let lookup_language = prefix_language.unwrap_or(&language);

    // Look up alias: try active stage, then fall back to live. One query
    // either way: exact (as requested or percent-decoded) or ignoring case.
    let found = lookup_alias(state.db(), &path, active_stage, lookup_language).await;
    let alias = match found.map(|found| decide(found, prefix_language)) {
        Some(Resolution::Redirect(target)) => {
            let location = match request.uri().query() {
                Some(query) => format!("{asked_prefix}{}?{query}", location_path(&target)),
                None => format!("{asked_prefix}{}", location_path(&target)),
            };
            return axum::response::Redirect::permanent(&location).into_response();
        }
        Some(Resolution::Serve(alias)) => Some(alias),
        None => None,
    };

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

    // No alias found. The stripped path may still name a real route, so forward
    // it to the inner router rather than 404 here: `/it/search` is `/search`
    // read in Italian, not a missing page.
    //
    // Whether a prefix was there cannot be answered from this URI.
    // `negotiate_language` strips one off every request it handles, which is too
    // late to affect route matching (axum 0.8 runs `Router::layer` middleware
    // *after* the route is chosen) but early enough that `strip_language_prefix`
    // above finds nothing left to strip. So `had_lang_prefix` is false for every
    // request that reached here through that middleware, which is all of them,
    // and this branch never ran: every language-prefixed address that was not a
    // content alias 404'd, the front page at `/it/` included.
    //
    // `RequestedPath` is recorded before that strip and is the only surviving
    // record of the address the visitor asked for. Both conditions are kept: the
    // local strip is what a caller that never ran the middleware relies on.
    let stripped_upstream = request
        .extensions()
        .get::<crate::middleware::RequestedPath>()
        .is_some_and(|asked| asked.0 != raw_path);

    if ((had_lang_prefix && path != raw_path) || stripped_upstream)
        && let Ok(new_uri) = rewrite_uri(request.uri(), &path)
    {
        *request.uri_mut() = new_uri;
        return router
            .as_ref()
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|err| match err {});
    }

    // No alias and no language prefix: the one 404 page.
    crate::routes::helpers::render_not_found()
}

/// Strip a language prefix from the path if it matches a known language, the
/// default included (`/en/about` is `/about` in English; whether that address
/// is served or redirected is `negotiate_language`'s decision).
///
/// Returns `(stripped_path, Some(language_code))` if a prefix was found,
/// or `(original_path, None)` if no prefix matches.
pub(crate) fn strip_language_prefix(
    path: &str,
    known_languages: &[String],
) -> (String, Option<String>) {
    // Check if the path starts with /{lang}/ or is exactly /{lang}
    if let Some(trimmed) = path.strip_prefix('/') {
        let (candidate, rest) = if let Some(pos) = trimmed.find('/') {
            (&trimmed[..pos], &trimmed[pos..])
        } else {
            (trimmed, "/")
        };

        if known_languages.iter().any(|l| l == candidate) {
            return (rest.to_string(), Some(candidate.to_string()));
        }
    }

    (path.to_string(), None)
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
