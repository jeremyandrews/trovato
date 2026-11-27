//! Edge-cache headers for anonymous pages (B303).
//!
//! A Trovato site answers every page itself; nothing here adds a cache in
//! front of Postgres or Redis. What this middleware adds is the *header* a
//! CDN already in front of the site needs to cache safely: whether a response
//! may be stored and shared, for how long, and which tags it was built from,
//! so a later write can tell the CDN what to drop
//! ([`crate::cache::purge`]).
//!
//! # Placement
//!
//! Added in `main.rs` and `tests/common/mod.rs` outside the session layer (so
//! it runs after the session layer has added any `Set-Cookie` on the way
//! out) and inside the security-headers layer. `negotiate_language` runs
//! deeper still, so the language it resolved is not yet on the request when
//! this layer's pre-dispatch code runs; `negotiate_language` carries its
//! answer back out on the *response*'s extensions
//! ([`crate::middleware::language::LanguageSource`]), the same way
//! `track_request_timing` carries back a [`crate::telemetry::RequestProfileSnapshot`].
//!
//! # The decision
//!
//! A response the handler already marked with `Cache-Control` is left alone
//! (a feed, a file, `/health`, `/user/login`'s form page): this layer only
//! ever adds the header when the route did not ask for one.
//!
//! Otherwise, a response is `public, max-age=<EDGE_CACHE_MAX_AGE>,
//! s-maxage=<EDGE_CACHE_S_MAXAGE>` (with `Vary: Cookie` merged in, and the
//! tag header) only when **every** one of these holds:
//!
//! - `EDGE_CACHE_S_MAXAGE` is above zero (the public half's on/off switch);
//! - the method is `GET` or `HEAD`;
//! - the status is 200;
//! - the content type is `text/html`;
//! - the request carries no session cookie and no `Authorization` header;
//! - the response does not set a cookie;
//! - the language was resolved from the URL (a `/{lang}/...` prefix,
//!   explicit or the default's own) or, with no prefix, from the site's
//!   default *and* the site has only one language — never from a session
//!   override or `Accept-Language`, which can vary the answer at the exact
//!   same URL.
//!
//! Short of that, a response is `private, no-store` whenever the request
//! carried a session cookie or an `Authorization` header, the method was not
//! `GET`/`HEAD`, or the response set a cookie — every one of those means the
//! response is (or may be) particular to one visitor. Anything else (an
//! anonymous 404, a page whose language came from `Accept-Language`) gets no
//! `Cache-Control` from this layer at all, exactly as before B303.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use super::language::LanguageSource;
use crate::config::Lookup;
use crate::state::AppState;

/// Whether and how an anonymous page may be cached by a CDN.
///
/// `EDGE_CACHE_S_MAXAGE` is the public half's switch: zero (the default)
/// means no existing site starts being cached by a CDN on upgrade. The
/// private half (marking a signed-in, token-bearing or cookie-setting
/// response `private, no-store`) runs regardless, because it only stops a
/// shared cache from storing a response no correct cache should store.
#[derive(Debug, Clone)]
pub struct EdgeCacheConfig {
    /// `Cache-Control: max-age`, seconds a browser may keep a page itself
    /// (`EDGE_CACHE_MAX_AGE`, default 0).
    pub max_age_secs: u64,
    /// `Cache-Control: s-maxage`, seconds a shared cache (the CDN) may keep a
    /// page (`EDGE_CACHE_S_MAXAGE`, default 0). Above zero turns on the
    /// public half of this layer.
    pub s_maxage_secs: u64,
    /// The header a response's cache tags are sent under
    /// (`EDGE_CACHE_TAG_HEADER`, default `Surrogate-Key`; Cloudflare's
    /// purge-by-tag header. Use `Cache-Tag` for Fastly, `Edge-Cache-Tag` for
    /// Akamai).
    pub tag_header: HeaderName,
}

impl Default for EdgeCacheConfig {
    fn default() -> Self {
        Self {
            max_age_secs: 0,
            s_maxage_secs: 0,
            tag_header: HeaderName::from_static("surrogate-key"),
        }
    }
}

impl EdgeCacheConfig {
    /// Resolve the three settings, refusing a value that cannot be right.
    pub(crate) fn from_lookup(get: Lookup<'_>) -> anyhow::Result<Self> {
        let max_age_secs = Self::parse_secs(get, "EDGE_CACHE_MAX_AGE")?;
        let s_maxage_secs = Self::parse_secs(get, "EDGE_CACHE_S_MAXAGE")?;
        let tag_header = match get("EDGE_CACHE_TAG_HEADER") {
            Some(raw) if !raw.trim().is_empty() => {
                HeaderName::try_from(raw.trim()).map_err(|_| {
                    anyhow::anyhow!(
                        "EDGE_CACHE_TAG_HEADER ({raw:?}) is not a valid HTTP header name"
                    )
                })?
            }
            _ => HeaderName::from_static("surrogate-key"),
        };
        Ok(Self {
            max_age_secs,
            s_maxage_secs,
            tag_header,
        })
    }

    fn parse_secs(get: Lookup<'_>, name: &str) -> anyhow::Result<u64> {
        match get(name) {
            Some(raw) if !raw.trim().is_empty() => raw.trim().parse::<u64>().map_err(|_| {
                anyhow::anyhow!(
                    "{name} ({raw:?}) must be a non-negative whole number of seconds"
                )
            }),
            _ => Ok(0),
        }
    }

    /// Whether the public half is on at all.
    fn public_enabled(&self) -> bool {
        self.s_maxage_secs > 0
    }
}

/// Whether `headers` carries a cookie named `name`, in any `Cookie` header.
fn request_has_cookie(headers: &HeaderMap, name: &str) -> bool {
    headers.get_all(header::COOKIE).iter().any(|value| {
        value.to_str().is_ok_and(|raw| {
            raw.split(';').any(|pair| {
                pair.split_once('=')
                    .is_some_and(|(key, _)| key.trim() == name)
            })
        })
    })
}

/// Add `Cookie` to `headers`' `Vary`, keeping whatever the route already set
/// there and never repeating it.
fn merge_vary_cookie(headers: &mut HeaderMap) {
    let existing = headers
        .get(header::VARY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut parts: Vec<&str> = existing
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if !parts.iter().any(|p| p.eq_ignore_ascii_case("cookie")) {
        parts.push("Cookie");
    }
    if let Ok(value) = HeaderValue::from_str(&parts.join(", ")) {
        headers.insert(header::VARY, value);
    }
}

/// Whether `negotiate_language` resolved the language in a way that answers
/// the same for every anonymous visitor to this exact URL: from the URL
/// itself, or from the site's default when there is only one language to
/// choose from. A session override or `Accept-Language` can vary with the
/// visitor at the very same URL, so neither is safe to cache, and neither is
/// the absence of the extension altogether (fail closed).
fn language_safe_for_public(response: &Response, state: &AppState) -> bool {
    match response.extensions().get::<LanguageSource>() {
        Some(LanguageSource::UrlPrefix) => true,
        Some(LanguageSource::Default) => state.languages().known().len() <= 1,
        _ => false,
    }
}

/// Mark a response `public`/`private` and attach its cache tags.
pub async fn edge_cache_headers(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let config = state.runtime().edge_cache.clone();
    let method = request.method().clone();
    let has_session_cookie =
        request_has_cookie(request.headers(), crate::session::SESSION_COOKIE_NAME);
    let has_auth_header = request.headers().contains_key(header::AUTHORIZATION);

    let tag_collector = crate::cache::tags::TagCollector::new();
    let mut response = crate::cache::tags::scope(tag_collector.clone(), next.run(request)).await;

    // A route that already decided its own Cache-Control (a feed, a file, a
    // `no-store` route) is never second-guessed by this layer.
    if response.headers().contains_key(header::CACHE_CONTROL) {
        return response;
    }

    let sets_cookie = response.headers().contains_key(header::SET_COOKIE);
    let is_get_or_head = matches!(method, Method::GET | Method::HEAD);
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));

    let publicly_cacheable = config.public_enabled()
        && is_get_or_head
        && response.status() == StatusCode::OK
        && is_html
        && !has_session_cookie
        && !has_auth_header
        && !sets_cookie
        && language_safe_for_public(&response, &state);

    if publicly_cacheable {
        let value = format!(
            "public, max-age={}, s-maxage={}",
            config.max_age_secs, config.s_maxage_secs
        );
        if let Ok(header_value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(header::CACHE_CONTROL, header_value);
        }
        merge_vary_cookie(response.headers_mut());
        let tags = crate::cache::tags::header_value(&tag_collector.snapshot());
        if let Ok(tag_value) = HeaderValue::from_str(&tags) {
            response
                .headers_mut()
                .insert(config.tag_header.clone(), tag_value);
        }
        return response;
    }

    let particular_to_one_visitor =
        has_session_cookie || has_auth_header || !is_get_or_head || sets_cookie;
    if particular_to_one_visitor {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }

    response
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn request_has_cookie_finds_named_cookie_among_several() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "foo=bar; id=abc123; baz=qux".parse().unwrap());
        assert!(request_has_cookie(&headers, "id"));
        assert!(!request_has_cookie(&headers, "nope"));
    }

    #[test]
    fn request_has_cookie_false_with_no_cookie_header() {
        let headers = HeaderMap::new();
        assert!(!request_has_cookie(&headers, "id"));
    }

    #[test]
    fn merge_vary_cookie_adds_when_absent() {
        let mut headers = HeaderMap::new();
        merge_vary_cookie(&mut headers);
        assert_eq!(headers.get(header::VARY).unwrap(), "Cookie");
    }

    #[test]
    fn merge_vary_cookie_keeps_existing_and_adds_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(header::VARY, "Accept-Encoding".parse().unwrap());
        merge_vary_cookie(&mut headers);
        assert_eq!(headers.get(header::VARY).unwrap(), "Accept-Encoding, Cookie");
    }

    #[test]
    fn merge_vary_cookie_does_not_repeat_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(header::VARY, "cookie".parse().unwrap());
        merge_vary_cookie(&mut headers);
        assert_eq!(headers.get(header::VARY).unwrap(), "cookie");
    }

    #[test]
    fn from_lookup_defaults_to_disabled_and_surrogate_key() {
        let config = EdgeCacheConfig::from_lookup(&|_: &str| None).unwrap();
        assert_eq!(config.max_age_secs, 0);
        assert_eq!(config.s_maxage_secs, 0);
        assert!(!config.public_enabled());
        assert_eq!(config.tag_header.as_str(), "surrogate-key");
    }

    #[test]
    fn from_lookup_parses_every_setting() {
        let map = [
            ("EDGE_CACHE_MAX_AGE", "60"),
            ("EDGE_CACHE_S_MAXAGE", "600"),
            ("EDGE_CACHE_TAG_HEADER", "Cache-Tag"),
        ];
        let get = |name: &str| map.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string());
        let config = EdgeCacheConfig::from_lookup(&get).unwrap();
        assert_eq!(config.max_age_secs, 60);
        assert_eq!(config.s_maxage_secs, 600);
        assert!(config.public_enabled());
        assert_eq!(config.tag_header.as_str(), "cache-tag");
    }

    #[test]
    fn from_lookup_refuses_an_unparseable_max_age() {
        let get = |name: &str| (name == "EDGE_CACHE_MAX_AGE").then(|| "soon".to_string());
        let err = EdgeCacheConfig::from_lookup(&get).unwrap_err();
        assert!(err.to_string().contains("EDGE_CACHE_MAX_AGE"));
    }

    #[test]
    fn from_lookup_refuses_an_invalid_header_name() {
        let get = |name: &str| {
            (name == "EDGE_CACHE_TAG_HEADER").then(|| "not a header".to_string())
        };
        let err = EdgeCacheConfig::from_lookup(&get).unwrap_err();
        assert!(err.to_string().contains("EDGE_CACHE_TAG_HEADER"));
    }
}
