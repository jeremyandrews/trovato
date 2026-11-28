//! Edge caching headers (B303): tells a CDN whether a response may be shared,
//! for how long, and what it was built from.
//!
//! Added outside the session layer, so it sees the session layer's
//! `Set-Cookie`, and inside the security headers layer, in both `main.rs`
//! and `tests/common/mod.rs`. It opens a tag collector around `next.run`, so
//! every item load ([`crate::edge_cache::tags::record_item`]) and gather run
//! ([`crate::edge_cache::tags::record_items`]) on the request's own task
//! records itself, then decides the headers on the finished response.
//!
//! See `docs/deployment/cdn.md` for the full policy; `decide` (below) is
//! where it is pinned, as a pure function the real middleware is a thin
//! wrapper around (`crate::middleware::security_headers` is this shape too).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header},
    middleware::Next,
    response::Response,
};

use crate::edge_cache::tags::SITE;
use crate::middleware::language::{LanguageSource, ResolvedLanguageSource};
use crate::session::SESSION_COOKIE_NAME;
use crate::state::AppState;

/// `EDGE_CACHE_TAG_HEADER` default: Cloudflare's surrogate-key header name.
pub const DEFAULT_TAG_HEADER: &str = "Surrogate-Key";

/// Above this many bytes in the joined tag header, `item:` tags are dropped
/// and only `items`/`site` are kept: any write that would purge an `item:`
/// tag also purges `items`, so nothing goes stale.
const MAX_TAG_HEADER_BYTES: usize = 8 * 1024;

/// The settings this middleware reads (`EDGE_CACHE_MAX_AGE`,
/// `EDGE_CACHE_S_MAXAGE`, `EDGE_CACHE_TAG_HEADER`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeCacheConfig {
    /// `EDGE_CACHE_MAX_AGE`, seconds for browsers (default 0).
    pub max_age: u64,
    /// `EDGE_CACHE_S_MAXAGE`, seconds for shared caches (default 0, which
    /// keeps the public half off: see `decide`, below).
    pub s_maxage: u64,
    /// `EDGE_CACHE_TAG_HEADER` (default [`DEFAULT_TAG_HEADER`]).
    pub tag_header: String,
}

impl Default for EdgeCacheConfig {
    fn default() -> Self {
        Self {
            max_age: 0,
            s_maxage: 0,
            tag_header: DEFAULT_TAG_HEADER.to_string(),
        }
    }
}

impl EdgeCacheConfig {
    /// Resolve from a settings lookup, refusing a value that cannot be right.
    pub(crate) fn from_lookup(lookup: crate::config::Lookup<'_>) -> anyhow::Result<Self> {
        use anyhow::Context;

        let max_age = match lookup("EDGE_CACHE_MAX_AGE") {
            Some(raw) => raw
                .trim()
                .parse()
                .context("EDGE_CACHE_MAX_AGE must be a non-negative number of seconds")?,
            None => 0,
        };
        let s_maxage = match lookup("EDGE_CACHE_S_MAXAGE") {
            Some(raw) => raw
                .trim()
                .parse()
                .context("EDGE_CACHE_S_MAXAGE must be a non-negative number of seconds")?,
            None => 0,
        };
        // A value that is present but blank (or all whitespace) is a mistake
        // to report, not a reason to fall back to the default: only an
        // *absent* setting takes the default.
        let tag_header = match lookup("EDGE_CACHE_TAG_HEADER") {
            Some(raw) => raw.trim().to_string(),
            None => DEFAULT_TAG_HEADER.to_string(),
        };
        if !is_valid_header_token(&tag_header) {
            anyhow::bail!("EDGE_CACHE_TAG_HEADER ({tag_header:?}) is not a valid HTTP header name");
        }
        if RESERVED_TAG_HEADER_NAMES.contains(&tag_header.to_ascii_lowercase().as_str()) {
            anyhow::bail!(
                "EDGE_CACHE_TAG_HEADER ({tag_header:?}) would overwrite a header this response \
                 already sets"
            );
        }

        Ok(Self {
            max_age,
            s_maxage,
            tag_header,
        })
    }
}

/// Header names this middleware, or the response it decorates, already owns:
/// a tag header configured to one of these would silently overwrite it.
const RESERVED_TAG_HEADER_NAMES: &[&str] = &[
    "cache-control",
    "vary",
    "content-length",
    "content-type",
    "set-cookie",
];

/// Whether `s` is a legal HTTP header name (RFC 7230 `token`).
fn is_valid_header_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// What [`decide`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// The handler already set `Cache-Control`; leave the response as it is.
    LeaveAlone,
    /// `Cache-Control: public, max-age=<>, s-maxage=<>`, `Vary: Cookie` and the
    /// tag header.
    Public,
    /// `Cache-Control: private, no-store`.
    PrivateNoStore,
    /// Neither: an anonymous error response, or a page whose language a
    /// header chose. No `Cache-Control` from this layer, as before B303.
    Nothing,
}

/// Everything [`decide`] needs, gathered from the request and the response it
/// produced.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DecisionInput {
    /// `EDGE_CACHE_S_MAXAGE`; the public half is off at 0.
    pub s_maxage: u64,
    pub method_is_get_or_head: bool,
    pub status_is_200: bool,
    pub is_html: bool,
    pub has_session_cookie: bool,
    pub has_authorization: bool,
    pub sets_cookie: bool,
    /// Whether the request's language is the same for every visitor of this
    /// URL (a URL prefix, or the default language on a single-language site).
    pub language_publicly_cacheable: bool,
    pub handler_set_cache_control: bool,
}

/// The pure decision core (see module docs): fails closed. Anything not
/// proven anonymous, cookie-free, `GET`/`HEAD`, 200, HTML and
/// language-fixed-by-the-URL is never [`Decision::Public`].
pub(crate) fn decide(input: &DecisionInput) -> Decision {
    if input.handler_set_cache_control {
        return Decision::LeaveAlone;
    }

    let could_be_shared = input.method_is_get_or_head
        && input.status_is_200
        && input.is_html
        && !input.has_session_cookie
        && !input.has_authorization
        && !input.sets_cookie
        && input.language_publicly_cacheable;

    if input.s_maxage > 0 && could_be_shared {
        return Decision::Public;
    }

    // The private half is always on: it only stops a shared cache from
    // storing a personal response, which no correct cache should store
    // anyway, whatever EDGE_CACHE_S_MAXAGE is set to.
    if input.has_session_cookie
        || input.has_authorization
        || !input.method_is_get_or_head
        || input.sets_cookie
    {
        return Decision::PrivateNoStore;
    }

    Decision::Nothing
}

/// Whether `headers` carries a `Cookie` header naming `name`.
///
/// A request may legally repeat the `Cookie` field (split across more than
/// one header line), so this reads every one of them with [`HeaderMap::get_all`]
/// rather than only the first (`HeaderMap::get`): a session cookie carried
/// in a second `Cookie` line must still mark the response private.
fn request_has_cookie(headers: &HeaderMap, name: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|raw| {
            raw.split(';').any(|pair| {
                pair.split_once('=')
                    .map(|(k, _)| k.trim() == name)
                    .unwrap_or(false)
            })
        })
}

/// Merge `Cookie` into any existing `Vary` header, without duplicating it and
/// without dropping any other variation dimension a handler already set.
///
/// A handler may send more than one `Vary` field (rather than one
/// comma-joined value), so this reads every one of them with
/// [`HeaderMap::get_all`] and re-joins them into the single merged header
/// this function leaves behind.
fn merge_vary_cookie(headers: &mut HeaderMap) {
    const COOKIE: &str = "Cookie";
    let mut values: Vec<String> = Vec::new();
    for existing in headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
    {
        for part in existing.split(',') {
            let part = part.trim();
            if !part.is_empty() && !values.iter().any(|v| v.eq_ignore_ascii_case(part)) {
                values.push(part.to_string());
            }
        }
    }
    if !values.iter().any(|v| v.eq_ignore_ascii_case(COOKIE)) {
        values.push(COOKIE.to_string());
    }
    if let Ok(value) = HeaderValue::from_str(&values.join(", ")) {
        // `insert` (unlike `append`) replaces every existing value for this
        // header with the one merged value built above.
        headers.insert(header::VARY, value);
    }
}

/// Fastly and Varnish's `Surrogate-Key` convention separates keys with
/// whitespace, not commas: a comma is a legal character inside a key there,
/// so joining with one would merge every tag into a single, unpurgeable key.
/// Cloudflare's `Cache-Tag` and Akamai's `Edge-Cache-Tag` both split on
/// commas. The separator therefore follows the configured header's name, not
/// a single hardcoded choice.
fn tag_separator(tag_header: &str) -> &'static str {
    if tag_header.eq_ignore_ascii_case(DEFAULT_TAG_HEADER) {
        " "
    } else {
        ","
    }
}

/// The tag header's value: every collected tag plus [`SITE`], joined with
/// [`tag_separator`], or (over [`MAX_TAG_HEADER_BYTES`]) `items` and `site`
/// alone.
///
/// `items` is always kept in the overflow case, whether or not a gather ran:
/// once the per-item tags are dropped, the page's dependency on individual
/// items can no longer be named, so it falls back to depending on all of
/// them, the same fallback an actual gather-listed page already carries.
pub(crate) fn tags_header_value(collected: &BTreeSet<String>, tag_header: &str) -> String {
    let separator = tag_separator(tag_header);
    let mut tags = collected.clone();
    tags.insert(SITE.to_string());
    let joined = tags.iter().cloned().collect::<Vec<_>>().join(separator);
    if joined.len() <= MAX_TAG_HEADER_BYTES {
        return joined;
    }
    tags.insert(crate::edge_cache::tags::ITEMS.to_string());
    tags.iter()
        .filter(|t| !t.starts_with("item:"))
        .cloned()
        .collect::<Vec<_>>()
        .join(separator)
}

/// Whether a `Content-Type` value is exactly `text/html`, optionally with
/// parameters (`; charset=utf-8`), case-insensitively: a media type's type
/// and subtype are case-insensitive, and the subtype must match exactly, not
/// merely start with it (`text/html-example` is a different, made-up type).
fn is_html_content_type(ct: &str) -> bool {
    ct.split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
}

/// The middleware. See the module docs for where it goes in the stack.
pub async fn set_cache_headers(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method_is_get_or_head = matches!(*request.method(), Method::GET | Method::HEAD);
    let has_session_cookie = request_has_cookie(request.headers(), SESSION_COOKIE_NAME);
    let has_authorization = request.headers().contains_key(header::AUTHORIZATION);

    let collected: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let mut response = crate::edge_cache::tags::scope(collected.clone(), next.run(request)).await;

    let settings = &state.runtime().edge_cache;
    let handler_set_cache_control = response.headers().contains_key(header::CACHE_CONTROL);
    let status_is_200 = response.status() == StatusCode::OK;
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(is_html_content_type);
    let sets_cookie = response.headers().contains_key(header::SET_COOKIE);

    let single_language_site = state.languages().known().len() <= 1;
    let language_source = response
        .extensions()
        .get::<ResolvedLanguageSource>()
        .map(|s| s.0);
    let language_publicly_cacheable = matches!(language_source, Some(LanguageSource::UrlPrefix))
        || (matches!(language_source, Some(LanguageSource::Default)) && single_language_site);

    let decision = decide(&DecisionInput {
        s_maxage: settings.s_maxage,
        method_is_get_or_head,
        status_is_200,
        is_html,
        has_session_cookie,
        has_authorization,
        sets_cookie,
        language_publicly_cacheable,
        handler_set_cache_control,
    });

    match decision {
        Decision::LeaveAlone | Decision::Nothing => {}
        Decision::PrivateNoStore => {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
        }
        Decision::Public => {
            let cache_control = format!(
                "public, max-age={}, s-maxage={}",
                settings.max_age, settings.s_maxage
            );
            if let Ok(value) = HeaderValue::from_str(&cache_control) {
                response.headers_mut().insert(header::CACHE_CONTROL, value);
            }
            merge_vary_cookie(response.headers_mut());

            let collected = collected.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let header_value = tags_header_value(&collected, &settings.tag_header);
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(settings.tag_header.as_bytes()),
                HeaderValue::from_str(&header_value),
            ) {
                response.headers_mut().insert(name, value);
            }
        }
    }

    response
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn base() -> DecisionInput {
        DecisionInput {
            s_maxage: 600,
            method_is_get_or_head: true,
            status_is_200: true,
            is_html: true,
            has_session_cookie: false,
            has_authorization: false,
            sets_cookie: false,
            language_publicly_cacheable: true,
            handler_set_cache_control: false,
        }
    }

    #[test]
    fn an_anonymous_html_200_get_is_public() {
        assert_eq!(decide(&base()), Decision::Public);
    }

    #[test]
    fn a_handler_set_cache_control_is_left_alone() {
        let mut input = base();
        input.handler_set_cache_control = true;
        assert_eq!(decide(&input), Decision::LeaveAlone);
        // Even when every other condition would have made it public: the
        // route's own decision always wins.
        input.has_session_cookie = true;
        assert_eq!(decide(&input), Decision::LeaveAlone);
    }

    #[test]
    fn s_maxage_zero_never_makes_anything_public() {
        let mut input = base();
        input.s_maxage = 0;
        assert_eq!(decide(&input), Decision::Nothing);
    }

    #[test]
    fn a_session_cookie_is_always_private_no_store() {
        let mut input = base();
        input.has_session_cookie = true;
        assert_eq!(decide(&input), Decision::PrivateNoStore);
        // Even with the public half off: the private half is always on.
        input.s_maxage = 0;
        assert_eq!(decide(&input), Decision::PrivateNoStore);
    }

    #[test]
    fn an_authorization_header_is_always_private_no_store() {
        let mut input = base();
        input.has_authorization = true;
        assert_eq!(decide(&input), Decision::PrivateNoStore);
    }

    #[test]
    fn a_post_is_always_private_no_store() {
        let mut input = base();
        input.method_is_get_or_head = false;
        assert_eq!(decide(&input), Decision::PrivateNoStore);
    }

    #[test]
    fn a_response_that_sets_a_cookie_is_always_private_no_store() {
        let mut input = base();
        input.sets_cookie = true;
        assert_eq!(decide(&input), Decision::PrivateNoStore);
    }

    #[test]
    fn an_anonymous_404_is_never_public_and_may_carry_nothing() {
        let mut input = base();
        input.status_is_200 = false;
        assert_eq!(decide(&input), Decision::Nothing);
    }

    #[test]
    fn a_non_html_response_is_never_public() {
        let mut input = base();
        input.is_html = false;
        assert_eq!(decide(&input), Decision::Nothing);
    }

    #[test]
    fn a_header_negotiated_language_is_never_public() {
        let mut input = base();
        input.language_publicly_cacheable = false;
        assert_eq!(decide(&input), Decision::Nothing);
    }

    #[test]
    fn header_token_validation() {
        assert!(is_valid_header_token("Surrogate-Key"));
        assert!(is_valid_header_token("Cache-Tag"));
        assert!(is_valid_header_token("Edge-Cache-Tag"));
        assert!(!is_valid_header_token(""));
        assert!(!is_valid_header_token("Not A Header"));
        assert!(!is_valid_header_token("X-Evil:\r\nFoo"));
        assert!(!is_valid_header_token("Tag/Header"));
    }

    #[test]
    fn config_defaults_to_off() {
        let settings: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        let config =
            EdgeCacheConfig::from_lookup(&|name| settings.get(name).map(|v| (*v).to_string()))
                .expect("defaults parse");
        assert_eq!(config.max_age, 0);
        assert_eq!(config.s_maxage, 0);
        assert_eq!(config.tag_header, DEFAULT_TAG_HEADER);
    }

    #[test]
    fn config_reads_every_setting() {
        let settings: std::collections::HashMap<&str, &str> = [
            ("EDGE_CACHE_MAX_AGE", "60"),
            ("EDGE_CACHE_S_MAXAGE", "600"),
            ("EDGE_CACHE_TAG_HEADER", "Cache-Tag"),
        ]
        .into_iter()
        .collect();
        let config =
            EdgeCacheConfig::from_lookup(&|name| settings.get(name).map(|v| (*v).to_string()))
                .expect("valid settings parse");
        assert_eq!(config.max_age, 60);
        assert_eq!(config.s_maxage, 600);
        assert_eq!(config.tag_header, "Cache-Tag");
    }

    #[test]
    fn an_unparseable_max_age_refuses_to_start() {
        let settings: std::collections::HashMap<&str, &str> =
            [("EDGE_CACHE_MAX_AGE", "not-a-number")]
                .into_iter()
                .collect();
        let err =
            EdgeCacheConfig::from_lookup(&|name| settings.get(name).map(|v| (*v).to_string()))
                .expect_err("not a number must refuse to start");
        assert!(err.to_string().contains("EDGE_CACHE_MAX_AGE"));
    }

    #[test]
    fn an_invalid_tag_header_name_refuses_to_start() {
        let settings: std::collections::HashMap<&str, &str> =
            [("EDGE_CACHE_TAG_HEADER", "Not A Header")]
                .into_iter()
                .collect();
        let err =
            EdgeCacheConfig::from_lookup(&|name| settings.get(name).map(|v| (*v).to_string()))
                .expect_err("an invalid header name must refuse to start");
        assert!(err.to_string().contains("EDGE_CACHE_TAG_HEADER"));
    }

    #[test]
    fn a_whitespace_only_tag_header_refuses_to_start_rather_than_defaulting() {
        let settings: std::collections::HashMap<&str, &str> =
            [("EDGE_CACHE_TAG_HEADER", "   ")].into_iter().collect();
        let err =
            EdgeCacheConfig::from_lookup(&|name| settings.get(name).map(|v| (*v).to_string()))
                .expect_err("a blank, but present, setting must refuse to start");
        assert!(err.to_string().contains("EDGE_CACHE_TAG_HEADER"));
    }

    #[test]
    fn a_tag_header_that_would_overwrite_a_reserved_header_refuses_to_start() {
        for name in ["Vary", "Cache-Control", "content-length", "Content-Type"] {
            let settings: std::collections::HashMap<&str, &str> =
                [("EDGE_CACHE_TAG_HEADER", name)].into_iter().collect();
            let err = EdgeCacheConfig::from_lookup(&|n| settings.get(n).map(|v| (*v).to_string()))
                .expect_err(&format!("{name} must refuse to start"));
            assert!(err.to_string().contains("EDGE_CACHE_TAG_HEADER"));
        }
    }

    #[test]
    fn tags_header_includes_site_always() {
        let collected = BTreeSet::new();
        assert_eq!(tags_header_value(&collected, "Cache-Tag"), "site");
    }

    #[test]
    fn tags_header_joins_every_collected_tag() {
        let collected = BTreeSet::from(["items".to_string(), "item:abc".to_string()]);
        let value = tags_header_value(&collected, "Cache-Tag");
        assert!(value.contains("site"));
        assert!(value.contains("items"));
        assert!(value.contains("item:abc"));
    }

    #[test]
    fn the_default_surrogate_key_header_is_space_separated() {
        let collected = BTreeSet::from(["item:abc".to_string()]);
        let value = tags_header_value(&collected, DEFAULT_TAG_HEADER);
        // Fastly and Varnish split a `Surrogate-Key` value on whitespace, not
        // commas: joining with a comma would make every tag part of one
        // single, unpurgeable key.
        assert!(!value.contains(','), "{value:?} must not be comma joined");
        let parts: Vec<&str> = value.split(' ').collect();
        assert!(parts.contains(&"site"));
        assert!(parts.contains(&"item:abc"));
    }

    #[test]
    fn a_configured_cloudflare_style_header_is_comma_separated() {
        let collected = BTreeSet::from(["item:abc".to_string()]);
        let value = tags_header_value(&collected, "Cache-Tag");
        assert_eq!(value, "item:abc,site");
    }

    #[test]
    fn an_oversized_tag_header_drops_item_tags_but_keeps_items_and_site() {
        let mut collected = BTreeSet::new();
        for n in 0..2000 {
            collected.insert(format!("item:{n:032}"));
        }
        collected.insert("items".to_string());
        let value = tags_header_value(&collected, "Cache-Tag");
        assert!(
            value.len() <= MAX_TAG_HEADER_BYTES,
            "dropping item: tags must bring it under the cap"
        );
        assert!(value.contains("items"));
        assert!(value.contains("site"));
        assert!(!value.contains("item:"));
    }

    #[test]
    fn an_oversized_tag_header_keeps_items_even_when_no_gather_ran() {
        // A page that loaded enough individual items to overflow the cap, but
        // ran no gather query, still has nothing to name each item by once
        // the overflow fallback drops them: it must fall back to depending on
        // every item, the same as a gather-listed page, or an edit to any one
        // of those items would never purge it.
        let mut collected = BTreeSet::new();
        for n in 0..2000 {
            collected.insert(format!("item:{n:032}"));
        }
        let value = tags_header_value(&collected, "Cache-Tag");
        assert!(value.len() <= MAX_TAG_HEADER_BYTES);
        assert!(
            value.contains("items"),
            "items must be kept even without a prior gather: {value:?}"
        );
        assert!(!value.contains("item:"));
    }

    #[test]
    fn request_cookie_detection_matches_only_the_named_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=1; id=abc123; third=2"),
        );
        assert!(request_has_cookie(&headers, "id"));
        assert!(!request_has_cookie(&headers, "nonexistent"));
    }

    #[test]
    fn request_cookie_detection_checks_every_split_cookie_header() {
        // HTTP allows a request to repeat the `Cookie` field across more than
        // one header line; the session cookie landing in anything but the
        // first one must still be found.
        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, HeaderValue::from_static("other=1"));
        headers.append(header::COOKIE, HeaderValue::from_static("id=abc123"));
        assert!(request_has_cookie(&headers, "id"));
    }

    #[test]
    fn is_html_content_type_matches_only_the_exact_subtype_case_insensitively() {
        assert!(is_html_content_type("text/html"));
        assert!(is_html_content_type("Text/HTML; charset=utf-8"));
        assert!(is_html_content_type("  text/html ; charset=utf-8"));
        assert!(!is_html_content_type("text/html-example"));
        assert!(!is_html_content_type("application/json"));
        assert!(!is_html_content_type("text/plain"));
    }

    #[test]
    fn vary_cookie_is_added_once_and_merged_with_existing() {
        let mut headers = HeaderMap::new();
        merge_vary_cookie(&mut headers);
        assert_eq!(headers.get(header::VARY).unwrap(), "Cookie");

        let mut headers = HeaderMap::new();
        headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
        merge_vary_cookie(&mut headers);
        assert_eq!(
            headers.get(header::VARY).unwrap(),
            "Accept-Encoding, Cookie"
        );

        // Already present: unchanged, not duplicated.
        let mut headers = HeaderMap::new();
        headers.insert(header::VARY, HeaderValue::from_static("Cookie"));
        merge_vary_cookie(&mut headers);
        assert_eq!(headers.get(header::VARY).unwrap(), "Cookie");
    }

    #[test]
    fn vary_cookie_keeps_every_dimension_from_separate_vary_header_lines() {
        // A handler may send `Vary` as more than one header field rather than
        // one comma-joined value; merging in Cookie must not drop the others.
        let mut headers = HeaderMap::new();
        headers.append(header::VARY, HeaderValue::from_static("Accept"));
        headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
        merge_vary_cookie(&mut headers);
        let merged = headers.get(header::VARY).unwrap().to_str().unwrap();
        let parts: Vec<&str> = merged.split(',').map(str::trim).collect();
        assert!(parts.contains(&"Accept"), "{merged:?}");
        assert!(parts.contains(&"Accept-Encoding"), "{merged:?}");
        assert!(parts.contains(&"Cookie"), "{merged:?}");
        assert_eq!(headers.get_all(header::VARY).iter().count(), 1);
    }
}
