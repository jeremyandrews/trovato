//! Language negotiation middleware.
//!
//! Resolves the active language for each request using a chain of negotiators.
//! Resolution order: session override → URL prefix → Accept-Language → default.
//!
//! The URL prefix negotiator also strips the language prefix from the URI
//! so downstream middleware and routes see the clean path.

use std::collections::HashSet;

use axum::{
    body::Body,
    extract::State,
    http::{Request, Uri},
    middleware::Next,
    response::Response,
};
use tower_sessions::Session;

use crate::state::AppState;

/// Session key for storing the user's active language override.
pub const SESSION_ACTIVE_LANGUAGE: &str = "active_language";

/// The resolved language for the current request.
///
/// Stored in request extensions for per-request access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLanguage(pub String);

/// Trait for language negotiation strategies.
///
/// Implementations inspect the request and return a language code if they
/// can determine the desired language. The middleware chains negotiators
/// by priority (highest first) and uses the first match.
pub trait LanguageNegotiator: Send + Sync {
    /// Attempt to negotiate a language from the request.
    ///
    /// Returns `Some(language_code)` if this negotiator can determine the
    /// language, `None` otherwise.
    fn negotiate(&self, request: &Request<Body>) -> Option<String>;

    /// Priority of this negotiator (higher = checked first).
    fn priority(&self) -> i32;

    /// Combined negotiate + rewrite in a single pass.
    ///
    /// Returns `Some((language, optional_rewritten_path))` if this negotiator
    /// matches. Implementations should override for efficiency when both
    /// language and rewrite derive from the same parse.
    fn negotiate_with_rewrite(&self, request: &Request<Body>) -> Option<(String, Option<String>)> {
        let lang = self.negotiate(request)?;
        Some((lang, None))
    }
}

/// Negotiates language from URL prefix (e.g., `/fr/about` → language "fr", path "/about").
///
/// Uses a HashSet for O(1) language lookup. Only matches exact language codes
/// followed by `/` or end-of-path, preventing false matches like `/enterprise`.
///
/// The default language is excluded from prefix matching to prevent SEO
/// duplicate content (e.g., `/en/about` and `/about` would serve the same page).
/// Non-default languages require the prefix (e.g., `/fr/about`).
pub struct UrlPrefixNegotiator {
    known_languages: HashSet<String>,
    default_language: String,
}

impl UrlPrefixNegotiator {
    pub fn new(known_languages: Vec<String>, default_language: String) -> Self {
        Self {
            known_languages: known_languages.into_iter().collect(),
            default_language,
        }
    }

    /// Extract the language code from a URL prefix.
    ///
    /// Returns `Some((language, remaining_path))` if the path starts with
    /// a known non-default language prefix. The prefix must be followed by `/`
    /// or be the entire path. Uses O(1) HashSet lookup by extracting the first
    /// path segment as a candidate.
    ///
    /// The default language is skipped to prevent SEO duplicate content.
    pub fn extract_prefix<'a>(&self, path: &'a str) -> Option<(&str, &'a str)> {
        let trimmed = path.strip_prefix('/')?;

        // Extract first path segment as the candidate language code
        let (candidate, rest) = match trimmed.find('/') {
            Some(pos) => (&trimmed[..pos], &trimmed[pos..]),
            None => (trimmed, ""),
        };

        // Skip default language to prevent SEO duplicate content:
        // /en/about and /about should not both serve the same page.
        if candidate == self.default_language {
            return None;
        }

        // O(1) HashSet lookup instead of linear scan
        let lang = self.known_languages.get(candidate)?;

        if rest.is_empty() {
            // Bare prefix like "/fr" → language "fr", path "/"
            Some((lang, "/"))
        } else {
            // Prefix like "/fr/about" → language "fr", path "/about"
            Some((lang, rest))
        }
    }
}

impl LanguageNegotiator for UrlPrefixNegotiator {
    fn negotiate(&self, request: &Request<Body>) -> Option<String> {
        let path = request.uri().path();
        self.extract_prefix(path).map(|(lang, _)| lang.to_string())
    }

    fn priority(&self) -> i32 {
        100
    }

    /// Single-pass extraction of language and rewritten path from the URL prefix.
    fn negotiate_with_rewrite(&self, request: &Request<Body>) -> Option<(String, Option<String>)> {
        let path = request.uri().path();
        let (lang, remaining) = self.extract_prefix(path)?;
        Some((lang.to_string(), Some(remaining.to_string())))
    }
}

/// Negotiates language from the Accept-Language HTTP header.
///
/// Parses quality values and returns the highest-quality language that
/// matches a known language. Uses HashSet for O(1) lookup.
pub struct AcceptLanguageNegotiator {
    known_languages: HashSet<String>,
}

impl AcceptLanguageNegotiator {
    pub fn new(known_languages: Vec<String>) -> Self {
        Self {
            known_languages: known_languages.into_iter().collect(),
        }
    }

    /// Parse Accept-Language header value into (language, quality) pairs,
    /// sorted by quality descending (stable sort preserves original order for ties).
    fn parse_accept_language(header: &str) -> Vec<(String, f32)> {
        let mut langs: Vec<(String, f32)> = header
            .split(',')
            .filter_map(|part| {
                let part = part.trim();
                if part.is_empty() {
                    return None;
                }

                let mut segments = part.split(';');
                let lang = segments.next()?.trim().to_lowercase();

                let quality = segments
                    .find_map(|s| {
                        let s = s.trim();
                        s.strip_prefix("q=")
                            .and_then(|q| q.trim().parse::<f32>().ok())
                    })
                    .unwrap_or(1.0)
                    .clamp(0.0, 1.0); // RFC 7231 §5.3.1: quality values are 0.000–1.000

                Some((lang, quality))
            })
            .collect();

        // Stable sort: preserves original order for equal quality values
        langs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        langs
    }
}

impl LanguageNegotiator for AcceptLanguageNegotiator {
    fn negotiate(&self, request: &Request<Body>) -> Option<String> {
        let header = request.headers().get("accept-language")?.to_str().ok()?;

        let parsed = Self::parse_accept_language(header);

        for (lang, _quality) in parsed {
            // Check exact match first (O(1) HashSet lookup)
            if self.known_languages.contains(&lang) {
                return Some(lang);
            }
            // Check primary subtag (e.g., "en-US" → "en")
            if let Some(primary) = lang.split('-').next()
                && self.known_languages.contains(primary)
            {
                return Some(primary.to_string());
            }
        }

        None
    }

    fn priority(&self) -> i32 {
        50
    }
}

/// Middleware to negotiate the active language for each request.
///
/// Resolution order:
/// 1. Skip system paths (static files, API, health, install) — use default language
/// 2. Always strip URL language prefix if present (regardless of language source)
/// 3. Determine language: session override → URL prefix → Accept-Language → default
///
/// URL prefix stripping is decoupled from language resolution so that
/// `/en/about` is always rewritten to `/about`, even when the session
/// overrides the resolved language.
pub async fn negotiate_language(
    State(state): State<AppState>,
    session: Session,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let default_language = state.default_language();

    // Skip system paths that don't need language negotiation.
    //
    // This skip list is intentionally smaller than path_alias's skip list:
    // - /admin, /user, /item are NOT skipped here because we still need to
    //   strip URL language prefixes (e.g., /en/admin → /admin) and resolve
    //   the language for those routes.
    // - path_alias skips them because they never need alias-to-source rewriting.
    if path.starts_with("/static")
        || path.starts_with("/api")
        || path == "/health"
        || path.starts_with("/install")
    {
        request
            .extensions_mut()
            .insert(ResolvedLanguage(default_language.to_string()));
        return next.run(request).await;
    }

    let negotiators = state.language_negotiators();
    let known_languages = state.known_languages();

    // 1. Strip URL language prefix and capture the language in a single pass.
    //    Only negotiators that rewrite the URI (i.e., return Some(rewritten_path))
    //    set url_language. Non-rewriting negotiators (e.g., AcceptLanguage) are
    //    handled later in select_language via negotiate().
    let mut url_language: Option<String> = None;
    for negotiator in negotiators {
        if let Some((lang, rewritten)) = negotiator.negotiate_with_rewrite(&request)
            && let Some(rewritten) = rewritten
        {
            url_language = Some(lang);
            if let Ok(new_uri) = rewrite_uri_path(request.uri(), &rewritten) {
                tracing::debug!(
                    original = %request.uri(),
                    new_uri = %new_uri,
                    url_language = ?url_language,
                    "stripped language prefix from URI"
                );
                *request.uri_mut() = new_uri;
            }
            break;
        }
        // Non-rewriting negotiator matched but doesn't set url_language;
        // it will be consulted again via negotiate() in select_language.
    }

    // 2. Read session language (async) then select language (sync, testable).
    let session_lang: Option<String> = session
        .get::<Option<String>>(SESSION_ACTIVE_LANGUAGE)
        .await
        .ok()
        .flatten()
        .flatten();

    let language = select_language(
        session_lang.as_deref(),
        url_language.as_deref(),
        known_languages,
        negotiators,
        &request,
        default_language,
    );

    request.extensions_mut().insert(ResolvedLanguage(language));

    next.run(request).await
}

/// Select the active language from available sources (sync, testable).
///
/// Resolution order:
/// 1. Session override (validated against known languages)
/// 2. URL prefix language (validated against known languages)
/// 3. Remaining negotiators (Accept-Language, etc.)
/// 4. Default language
fn select_language(
    session_lang: Option<&str>,
    url_language: Option<&str>,
    known_languages: &[String],
    negotiators: &[std::sync::Arc<dyn LanguageNegotiator>],
    request: &Request<Body>,
    default_language: &str,
) -> String {
    // Check session override first, but validate against known languages
    if let Some(lang) = session_lang {
        if known_languages.iter().any(|k| k == lang) {
            return lang.to_string();
        }
        tracing::warn!(
            session_language = %lang,
            "session contains unknown language, ignoring"
        );
    }

    // Use URL prefix language (validated against known languages for consistency)
    if let Some(lang) = url_language {
        if known_languages.iter().any(|k| k == lang) {
            return lang.to_string();
        }
        tracing::warn!(
            url_language = %lang,
            "URL prefix contains unknown language, ignoring"
        );
    }

    // Try remaining negotiators by priority (already sorted desc).
    // Note: UrlPrefixNegotiator will not match here because the prefix was already
    // stripped by negotiate_with_rewrite in the middleware. Only non-rewriting
    // negotiators (e.g., AcceptLanguageNegotiator) can produce results here.
    for negotiator in negotiators {
        if let Some(lang) = negotiator.negotiate(request) {
            // Validate negotiator result against known languages (Fix #5)
            if known_languages.iter().any(|k| k == &lang) {
                return lang;
            }
            tracing::warn!(
                negotiator_language = %lang,
                "negotiator returned unknown language, ignoring"
            );
        }
    }

    // Fall back to default
    default_language.to_string()
}

/// Rewrite a URI to a new path while preserving query string.
fn rewrite_uri_path(original: &Uri, new_path: &str) -> Result<Uri, axum::http::uri::InvalidUri> {
    if let Some(query) = original.query() {
        format!("{new_path}?{query}").parse()
    } else {
        new_path.parse()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // --- UrlPrefixNegotiator tests ---

    #[test]
    fn url_prefix_matches_non_default_language_with_path() {
        // "en" is default, so only "fr" should match as a URL prefix
        let negotiator =
            UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()], "en".to_string());

        let result = negotiator.extract_prefix("/fr/page/123");
        assert_eq!(result, Some(("fr", "/page/123")));
    }

    #[test]
    fn url_prefix_matches_bare_non_default_language() {
        let negotiator =
            UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()], "en".to_string());

        let result = negotiator.extract_prefix("/fr");
        assert_eq!(result, Some(("fr", "/")));
    }

    #[test]
    fn url_prefix_skips_default_language() {
        // Default language prefix should NOT match to prevent SEO duplicate content
        let negotiator =
            UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()], "en".to_string());

        assert_eq!(negotiator.extract_prefix("/en/about"), None);
        assert_eq!(negotiator.extract_prefix("/en"), None);
    }

    #[test]
    fn url_prefix_does_not_match_enterprise() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()], "en".to_string());

        let result = negotiator.extract_prefix("/enterprise");
        assert_eq!(result, None);
    }

    #[test]
    fn url_prefix_does_not_match_unknown_language() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()], "en".to_string());

        let result = negotiator.extract_prefix("/de/page");
        assert_eq!(result, None);
    }

    #[test]
    fn url_prefix_no_match_root() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()], "en".to_string());

        let result = negotiator.extract_prefix("/");
        assert_eq!(result, None);
    }

    #[test]
    fn url_prefix_case_sensitive_no_match() {
        // URL prefix matching is case-sensitive: /EN/about should NOT match "en"
        let negotiator =
            UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()], "en".to_string());

        assert_eq!(negotiator.extract_prefix("/EN/about"), None);
        assert_eq!(negotiator.extract_prefix("/FR/about"), None);
        assert_eq!(negotiator.extract_prefix("/Fr/about"), None);
    }

    #[test]
    fn url_prefix_negotiate_with_rewrite_returns_both() {
        let negotiator =
            UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()], "en".to_string());

        let request = Request::builder()
            .uri("/fr/about")
            .body(Body::empty())
            .unwrap();

        let result = negotiator.negotiate_with_rewrite(&request);
        assert_eq!(result, Some(("fr".to_string(), Some("/about".to_string()))));
    }

    #[test]
    fn url_prefix_negotiate_with_rewrite_bare() {
        let negotiator =
            UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()], "en".to_string());

        let request = Request::builder().uri("/fr").body(Body::empty()).unwrap();

        let result = negotiator.negotiate_with_rewrite(&request);
        assert_eq!(result, Some(("fr".to_string(), Some("/".to_string()))));
    }

    #[test]
    fn url_prefix_negotiate_with_rewrite_no_match() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()], "en".to_string());

        let request = Request::builder()
            .uri("/about")
            .body(Body::empty())
            .unwrap();

        assert_eq!(negotiator.negotiate_with_rewrite(&request), None);
    }

    // --- AcceptLanguageNegotiator tests ---

    #[test]
    fn accept_language_negotiate_with_rewrite_returns_none_rewrite() {
        let negotiator = AcceptLanguageNegotiator::new(vec!["en".to_string()]);

        let request = Request::builder()
            .header("accept-language", "en")
            .body(Body::empty())
            .unwrap();

        // Accept-Language negotiator never rewrites paths
        let result = negotiator.negotiate_with_rewrite(&request);
        assert_eq!(result, Some(("en".to_string(), None)));
    }

    #[test]
    fn accept_language_parses_simple() {
        let parsed = AcceptLanguageNegotiator::parse_accept_language("en");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "en");
        assert_eq!(parsed[0].1, 1.0);
    }

    #[test]
    fn accept_language_parses_quality_values() {
        let parsed =
            AcceptLanguageNegotiator::parse_accept_language("fr;q=0.9, en;q=1.0, de;q=0.5");
        assert_eq!(parsed.len(), 3);
        // Should be sorted by quality descending
        assert_eq!(parsed[0].0, "en");
        assert_eq!(parsed[1].0, "fr");
        assert_eq!(parsed[2].0, "de");
    }

    #[test]
    fn accept_language_preserves_order_for_equal_quality() {
        // Both have implicit q=1.0 — stable sort should preserve original order
        let parsed = AcceptLanguageNegotiator::parse_accept_language("fr, en");
        assert_eq!(parsed[0].0, "fr");
        assert_eq!(parsed[1].0, "en");
    }

    #[test]
    fn accept_language_matches_known_language() {
        let negotiator = AcceptLanguageNegotiator::new(vec!["en".to_string(), "fr".to_string()]);

        let request = Request::builder()
            .header("accept-language", "de, fr;q=0.9, en;q=0.8")
            .body(Body::empty())
            .unwrap();

        // "de" is unknown, so should fall back to "fr" (next highest quality)
        let result = negotiator.negotiate(&request);
        assert_eq!(result, Some("fr".to_string()));
    }

    #[test]
    fn accept_language_matches_primary_subtag() {
        let negotiator = AcceptLanguageNegotiator::new(vec!["en".to_string()]);

        let request = Request::builder()
            .header("accept-language", "en-US;q=0.9")
            .body(Body::empty())
            .unwrap();

        let result = negotiator.negotiate(&request);
        assert_eq!(result, Some("en".to_string()));
    }

    #[test]
    fn accept_language_no_match_returns_none() {
        let negotiator = AcceptLanguageNegotiator::new(vec!["en".to_string()]);

        let request = Request::builder()
            .header("accept-language", "ja, zh;q=0.9")
            .body(Body::empty())
            .unwrap();

        let result = negotiator.negotiate(&request);
        assert_eq!(result, None);
    }

    #[test]
    fn accept_language_no_header_returns_none() {
        let negotiator = AcceptLanguageNegotiator::new(vec!["en".to_string()]);

        let request = Request::builder().body(Body::empty()).unwrap();

        let result = negotiator.negotiate(&request);
        assert_eq!(result, None);
    }

    #[test]
    fn accept_language_quality_clamped_to_rfc_range() {
        // RFC 7231 §5.3.1: quality values are 0.000–1.000
        // Out-of-range values should be clamped
        let parsed =
            AcceptLanguageNegotiator::parse_accept_language("en;q=1.5, fr;q=-0.5, de;q=0.5");
        assert_eq!(parsed.len(), 3);
        // en q=1.5 should be clamped to 1.0, fr q=-0.5 should be clamped to 0.0
        assert_eq!(parsed[0].0, "en");
        assert_eq!(parsed[0].1, 1.0);
        assert_eq!(parsed[1].0, "de");
        assert_eq!(parsed[1].1, 0.5);
        assert_eq!(parsed[2].0, "fr");
        assert_eq!(parsed[2].1, 0.0);
    }

    // --- select_language tests ---

    fn known() -> Vec<String> {
        vec!["en".to_string(), "fr".to_string(), "de".to_string()]
    }

    fn empty_request() -> Request<Body> {
        Request::builder().body(Body::empty()).unwrap()
    }

    #[test]
    fn select_language_session_valid() {
        let langs = known();
        let req = empty_request();
        let result = select_language(Some("fr"), None, &langs, &[], &req, "en");
        assert_eq!(result, "fr");
    }

    #[test]
    fn select_language_session_unknown_falls_through() {
        let langs = known();
        let req = empty_request();
        // Session has "xx" which is not in known — should fall to default
        let result = select_language(Some("xx"), None, &langs, &[], &req, "en");
        assert_eq!(result, "en");
    }

    #[test]
    fn select_language_url_takes_precedence_over_negotiators() {
        let langs = known();
        let negotiators: Vec<std::sync::Arc<dyn LanguageNegotiator>> =
            vec![Arc::new(AcceptLanguageNegotiator::new(vec![
                "en".to_string(),
                "de".to_string(),
            ]))];
        // Request has Accept-Language: de, but URL says "fr"
        let req = Request::builder()
            .header("accept-language", "de")
            .body(Body::empty())
            .unwrap();
        let result = select_language(None, Some("fr"), &langs, &negotiators, &req, "en");
        assert_eq!(result, "fr");
    }

    #[test]
    fn select_language_url_unknown_falls_through_to_negotiator() {
        let langs = known();
        let negotiators: Vec<std::sync::Arc<dyn LanguageNegotiator>> =
            vec![Arc::new(AcceptLanguageNegotiator::new(vec![
                "en".to_string(),
                "de".to_string(),
            ]))];
        let req = Request::builder()
            .header("accept-language", "de")
            .body(Body::empty())
            .unwrap();
        // URL has "xx" which is unknown — should fall through to Accept-Language "de"
        let result = select_language(None, Some("xx"), &langs, &negotiators, &req, "en");
        assert_eq!(result, "de");
    }

    #[test]
    fn select_language_session_beats_url() {
        let langs = known();
        let req = empty_request();
        // Both session and URL set, session wins
        let result = select_language(Some("de"), Some("fr"), &langs, &[], &req, "en");
        assert_eq!(result, "de");
    }

    #[test]
    fn select_language_default_fallback() {
        let langs = known();
        let req = empty_request();
        // Nothing set — falls to default
        let result = select_language(None, None, &langs, &[], &req, "en");
        assert_eq!(result, "en");
    }

    #[test]
    fn select_language_negotiator_unknown_lang_ignored() {
        // Negotiator returns a language not in known_languages — should fall to default
        let langs = vec!["en".to_string(), "fr".to_string()];
        let negotiators: Vec<std::sync::Arc<dyn LanguageNegotiator>> =
            vec![Arc::new(AcceptLanguageNegotiator::new(vec![
                "ja".to_string(), // negotiator knows "ja" but site doesn't
            ]))];
        let req = Request::builder()
            .header("accept-language", "ja")
            .body(Body::empty())
            .unwrap();
        let result = select_language(None, None, &langs, &negotiators, &req, "en");
        assert_eq!(result, "en");
    }

    // --- Other tests ---

    use std::sync::Arc;

    #[test]
    fn resolved_language_clone_and_eq() {
        let lang = ResolvedLanguage("en".to_string());
        let cloned = lang.clone();
        assert_eq!(lang, cloned);
        assert_eq!(lang, ResolvedLanguage("en".to_string()));
        assert_ne!(lang, ResolvedLanguage("fr".to_string()));
    }

    #[test]
    fn rewrite_uri_preserves_query() {
        let original: Uri = "/en/about?foo=bar".parse().unwrap();
        let result = rewrite_uri_path(&original, "/about").unwrap();
        assert_eq!(result.path(), "/about");
        assert_eq!(result.query(), Some("foo=bar"));
    }

    #[test]
    fn rewrite_uri_no_query() {
        let original: Uri = "/en/about".parse().unwrap();
        let result = rewrite_uri_path(&original, "/about").unwrap();
        assert_eq!(result.path(), "/about");
        assert_eq!(result.query(), None);
    }
}
