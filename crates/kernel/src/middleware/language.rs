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
    response::{IntoResponse, Response},
};
use tower_sessions::Session;

use crate::state::AppState;

/// Session key for storing the user's active language override.
pub const SESSION_ACTIVE_LANGUAGE: &str = "active_language";

/// ISO 639-1 language codes for right-to-left scripts.
///
/// Used to set `dir="rtl"` on the `<html>` element for proper text rendering.
pub const RTL_LANGUAGES: &[&str] = &[
    "ar",  // Arabic
    "arc", // Aramaic
    "ckb", // Central Kurdish (Sorani)
    "dv",  // Divehi
    "fa",  // Farsi (Persian)
    "ha",  // Hausa (Ajami script)
    "he",  // Hebrew
    "khw", // Khowar
    "ks",  // Kashmiri
    "ku",  // Kurdish
    "ps",  // Pashto
    "sd",  // Sindhi
    "ug",  // Uyghur
    "ur",  // Urdu
    "yi",  // Yiddish
];

/// Returns the text direction for a given language code.
///
/// Returns `"rtl"` for right-to-left languages, `"ltr"` for all others.
pub fn text_direction_for_language(lang: &str) -> &'static str {
    // Check the primary language subtag (e.g., "ar" from "ar-SA")
    let primary = lang.split('-').next().unwrap_or(lang);
    if RTL_LANGUAGES.contains(&primary) {
        "rtl"
    } else {
        "ltr"
    }
}

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
/// The default language's prefix is read too (`/en/about` is `/about` in
/// English): an old link that carried it keeps working. Whether `/en/about`
/// is then served or sent to `/about` is [`negotiate_language`]'s decision,
/// from [`LanguageSet::prefix_default`], so one page never has two addresses.
/// Non-default languages require the prefix (e.g., `/fr/about`).
pub struct UrlPrefixNegotiator {
    known_languages: HashSet<String>,
}

impl UrlPrefixNegotiator {
    pub fn new(known_languages: Vec<String>) -> Self {
        Self {
            known_languages: known_languages.into_iter().collect(),
        }
    }

    /// Extract the language code from a URL prefix.
    ///
    /// Returns `Some((language, remaining_path))` if the path starts with a
    /// known language prefix, the default language's included. The prefix
    /// must be followed by `/` or be the entire path. Uses O(1) HashSet lookup
    /// by extracting the first path segment as a candidate.
    pub fn extract_prefix<'a>(&self, path: &'a str) -> Option<(&str, &'a str)> {
        let trimmed = path.strip_prefix('/')?;

        // Extract first path segment as the candidate language code
        let (candidate, rest) = match trimmed.find('/') {
            Some(pos) => (&trimmed[..pos], &trimmed[pos..]),
            None => (trimmed, ""),
        };

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

/// The site key that serves the default language under its own prefix too
/// (`/en/...`), and makes every link the site writes carry it (B161).
pub const PREFIX_DEFAULT_KEY: &str = "language_prefix_default";

/// The site's languages as this server last read them: the codes in weight
/// order, the default, whether the default is prefixed, and the negotiator
/// chain built from them.
///
/// Held in [`AppState`] behind a lock and replaced whole when the languages
/// page changes anything ([`AppState::reload_languages`]), so a language
/// added in the admin is served at once, on every server (the change bumps
/// [`crate::cache::generation::CacheKind::Languages`]).
pub struct LanguageSet {
    known: std::sync::Arc<[String]>,
    default: String,
    prefix_default: bool,
    negotiators: Vec<std::sync::Arc<dyn LanguageNegotiator>>,
}

impl LanguageSet {
    /// Build the set and its negotiator chain.
    pub fn new(known: Vec<String>, default: String, prefix_default: bool) -> Self {
        let mut negotiators: Vec<std::sync::Arc<dyn LanguageNegotiator>> = vec![
            std::sync::Arc::new(UrlPrefixNegotiator::new(known.clone())),
            std::sync::Arc::new(AcceptLanguageNegotiator::new(known.clone())),
        ];
        negotiators.sort_by_key(|n| std::cmp::Reverse(n.priority()));
        Self {
            known: known.into(),
            default,
            prefix_default,
            negotiators,
        }
    }

    /// Read the languages and the prefix setting from the database.
    pub async fn load(pool: &sqlx::PgPool) -> anyhow::Result<Self> {
        let languages = crate::models::Language::list_all(pool).await?;
        let known: Vec<String> = languages.iter().map(|l| l.id.clone()).collect();
        let default = languages
            .iter()
            .find(|l| l.is_default)
            .map(|l| l.id.clone())
            .unwrap_or_else(|| "en".to_string());
        let prefix_default = crate::models::SiteConfig::get(pool, PREFIX_DEFAULT_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(Self::new(known, default, prefix_default))
    }

    /// The language codes, in weight order.
    pub fn known(&self) -> std::sync::Arc<[String]> {
        self.known.clone()
    }

    /// The default language's code.
    pub fn default_language(&self) -> &str {
        &self.default
    }

    /// Whether the default language is served and linked under its prefix.
    pub fn prefix_default(&self) -> bool {
        self.prefix_default
    }

    /// The negotiator chain, highest priority first.
    pub fn negotiators(&self) -> &[std::sync::Arc<dyn LanguageNegotiator>] {
        &self.negotiators
    }

    /// The prefix the site writes before a path in `language`: `/{language}`,
    /// or nothing for the default language unless it is prefixed.
    pub fn prefix(&self, language: &str) -> String {
        if language == self.default && !self.prefix_default {
            String::new()
        } else {
            format!("/{language}")
        }
    }
}

/// Whether a local path is served under every language's prefix.
///
/// Everything [`negotiate_language`] handles is: it strips `/{lang}` off the
/// request, and the alias fallback serves what is left in that language, so
/// `/it/recipes` is `/recipes` read in Italian. The system paths it passes
/// through untouched (static files, the API, health probes, the installer) are
/// not, and `/it/static/...` is not an address. The menu asks this too, so a
/// link is prefixed exactly when the prefixed address is one the site serves.
pub fn is_served_per_language(path: &str) -> bool {
    !(path.starts_with("/static")
        || path.starts_with("/api")
        || crate::routes::health::is_probe_path(path)
        || path.starts_with("/install"))
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
    // Record the address as asked for, before this function strips a language
    // prefix off it and before the alias fallback replaces an alias with its
    // source. Nothing downstream can reconstruct it: by the time a handler runs,
    // `/it/why` is `/item/{uuid}`. See [`RequestedPath`](crate::middleware::RequestedPath).
    //
    // Unless it is already recorded: the front page dispatch replaces `/` with
    // the configured path before this runs, and records `/` first.
    if request
        .extensions()
        .get::<crate::middleware::RequestedPath>()
        .is_none()
    {
        let requested_path = request.uri().path().to_string();
        request
            .extensions_mut()
            .insert(crate::middleware::RequestedPath(requested_path));
    }

    let path = request.uri().path();
    let languages = state.languages();
    let default_language = languages.default_language();

    // Skip system paths that don't need language negotiation.
    //
    // This skip list is intentionally smaller than path_alias's skip list:
    // - /admin, /user, /item are NOT skipped here because we still need to
    //   strip URL language prefixes (e.g., /en/admin → /admin) and resolve
    //   the language for those routes.
    // - path_alias skips them because they never need alias-to-source rewriting.
    if !is_served_per_language(path) {
        request
            .extensions_mut()
            .insert(ResolvedLanguage(default_language.to_string()));
        return next.run(request).await;
    }

    let negotiators = languages.negotiators();
    let known_languages = languages.known();

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

    // An old link with the default language's prefix (`/en/about`) on a site
    // that does not prefix it: one permanent redirect to the unprefixed
    // address, so the link works and the page keeps one address (B161). A
    // form post is served where it was sent: a redirect would drop its body.
    if url_language.as_deref() == Some(default_language)
        && !languages.prefix_default()
        && matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        )
    {
        let asked = request
            .extensions()
            .get::<crate::middleware::RequestedPath>()
            .map(|p| p.0.clone())
            .unwrap_or_default();
        let bare = without_prefix(&asked, default_language);
        let location = match request.uri().query() {
            Some(query) => format!("{bare}?{query}"),
            None => bare.to_string(),
        };
        return axum::response::Redirect::permanent(&location).into_response();
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
        &known_languages,
        negotiators,
        &request,
        default_language,
    );

    request.extensions_mut().insert(ResolvedLanguage(language));

    next.run(request).await
}

/// `path` without a leading `/{language}`: `/en/about` is `/about`, `/en` is
/// `/`.
fn without_prefix<'a>(path: &'a str, language: &str) -> &'a str {
    match path
        .strip_prefix('/')
        .and_then(|p| p.strip_prefix(language))
    {
        Some("") | Some("/") => "/",
        Some(rest) if rest.starts_with('/') => rest,
        _ => path,
    }
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
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()]);

        let result = negotiator.extract_prefix("/fr/page/123");
        assert_eq!(result, Some(("fr", "/page/123")));
    }

    #[test]
    fn url_prefix_matches_bare_non_default_language() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()]);

        let result = negotiator.extract_prefix("/fr");
        assert_eq!(result, Some(("fr", "/")));
    }

    #[test]
    fn url_prefix_reads_the_default_language_too() {
        // An old `/en/...` link names English; whether it is served there or
        // sent to the unprefixed address is the middleware's decision (B161).
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()]);

        assert_eq!(
            negotiator.extract_prefix("/en/about"),
            Some(("en", "/about"))
        );
        assert_eq!(negotiator.extract_prefix("/en"), Some(("en", "/")));
    }

    #[test]
    fn the_default_language_is_prefixed_only_when_the_site_says_so() {
        let known = vec!["en".to_string(), "es".to_string()];
        let plain = LanguageSet::new(known.clone(), "en".to_string(), false);
        assert_eq!(plain.prefix("en"), "");
        assert_eq!(plain.prefix("es"), "/es");
        let prefixed = LanguageSet::new(known, "en".to_string(), true);
        assert_eq!(prefixed.prefix("en"), "/en");
        assert_eq!(prefixed.prefix("es"), "/es");
    }

    #[test]
    fn url_prefix_does_not_match_enterprise() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()]);

        let result = negotiator.extract_prefix("/enterprise");
        assert_eq!(result, None);
    }

    #[test]
    fn url_prefix_does_not_match_unknown_language() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()]);

        let result = negotiator.extract_prefix("/de/page");
        assert_eq!(result, None);
    }

    #[test]
    fn url_prefix_no_match_root() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()]);

        let result = negotiator.extract_prefix("/");
        assert_eq!(result, None);
    }

    #[test]
    fn url_prefix_case_sensitive_no_match() {
        // URL prefix matching is case-sensitive: /EN/about should NOT match "en"
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()]);

        assert_eq!(negotiator.extract_prefix("/EN/about"), None);
        assert_eq!(negotiator.extract_prefix("/FR/about"), None);
        assert_eq!(negotiator.extract_prefix("/Fr/about"), None);
    }

    #[test]
    fn url_prefix_negotiate_with_rewrite_returns_both() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()]);

        let request = Request::builder()
            .uri("/fr/about")
            .body(Body::empty())
            .unwrap();

        let result = negotiator.negotiate_with_rewrite(&request);
        assert_eq!(result, Some(("fr".to_string(), Some("/about".to_string()))));
    }

    #[test]
    fn url_prefix_negotiate_with_rewrite_bare() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string(), "fr".to_string()]);

        let request = Request::builder().uri("/fr").body(Body::empty()).unwrap();

        let result = negotiator.negotiate_with_rewrite(&request);
        assert_eq!(result, Some(("fr".to_string(), Some("/".to_string()))));
    }

    #[test]
    fn url_prefix_negotiate_with_rewrite_no_match() {
        let negotiator = UrlPrefixNegotiator::new(vec!["en".to_string()]);

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
