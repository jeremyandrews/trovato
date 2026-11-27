//! Security response headers middleware.
//!
//! Injects Content-Security-Policy, X-Frame-Options, HSTS, and other
//! security headers on all responses. CSP prevents XSS even when
//! sanitization is bypassed. Other headers protect against clickjacking,
//! MIME sniffing, and downgrade attacks.

use std::net::SocketAddr;
use std::sync::LazyLock;

use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Request},
    middleware::Next,
    response::Response,
};
use regex::Regex;

use crate::client_addr::forwarded_https;
use crate::config::Lookup;
use crate::state::AppState;

/// Static header values — constructed at compile time from string literals.
static DENY: HeaderValue = HeaderValue::from_static("DENY");
static NOSNIFF: HeaderValue = HeaderValue::from_static("nosniff");
static REFERRER: HeaderValue = HeaderValue::from_static("strict-origin-when-cross-origin");
static NO_REFERRER: HeaderValue = HeaderValue::from_static("no-referrer");
static PERMISSIONS: HeaderValue =
    HeaderValue::from_static("camera=(), microphone=(), geolocation=()");
static HSTS: HeaderValue = HeaderValue::from_static("max-age=31536000; includeSubDomains");

/// The default CSP policy.
///
/// `script-src` carries no `'unsafe-inline'`, nonce or hash, so the browser
/// refuses every inline `<script>` block and inline `on*=` handler. Executable
/// JavaScript has to be a file under `static/js/`, loaded with `src`.
/// Template-dependent data is passed via `<script type="application/json">`
/// data blocks (which are not subject to CSP because they don't execute).
///
/// Not every stock template obeys that yet: the ones that still carry inline
/// scripts or handlers are tracked as BL-08 in `docs/BACKLOG.md`, and those
/// scripts do not run under this policy.
///
/// `style-src` keeps `'unsafe-inline'` because 100+ inline `style=`
/// attributes would break without it; inline styles are a low XSS risk.
static DEFAULT_CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'self'; \
     script-src 'self' 'wasm-unsafe-eval' https://cdn.jsdelivr.net; \
     style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
     img-src 'self' data:; \
     font-src 'self' https://fonts.gstatic.com; \
     connect-src 'self'; \
     frame-ancestors 'none'",
);

/// The header name for an enforcing policy.
const CSP_ENFORCE: &str = "content-security-policy";

/// The header name for a report-only policy.
const CSP_REPORT_ONLY: &str = "content-security-policy-report-only";

/// A CSP host source the kernel accepts from a site's `CSP_FRAME_SRC` or
/// `CSP_IMG_SRC` setting: an `http` or `https` URL naming a host, with an
/// optional `*.` wildcard subdomain, and nothing else — no port, no path, no
/// query, no fragment, no quote. This refuses `*` (no host at all), `data:`
/// and `'unsafe-inline'` (no `://`) along with anything carrying a path, by
/// construction rather than as special cases.
// Infallible: hard-coded valid regex pattern — Regex::new cannot fail here.
#[allow(clippy::expect_used)]
static CSP_HOST_SOURCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^https?://(\*\.)?[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$",
    )
    .expect("hard-coded regex")
});

/// Whether `source` is a CSP host source a site may add to `frame-src` or
/// `img-src` (see [`CSP_HOST_SOURCE_RE`]).
fn is_valid_csp_host_source(source: &str) -> bool {
    CSP_HOST_SOURCE_RE.is_match(source)
}

/// The host sources a site configured for `name` (`CSP_FRAME_SRC` or
/// `CSP_IMG_SRC`): comma or whitespace separated, each checked with
/// [`is_valid_csp_host_source`]. An entry that fails is dropped with a
/// warning — the rest of the list, and the response, are not lost over one
/// bad entry — and a setting that is absent or holds no valid entry yields an
/// empty list, which leaves the directive it would extend untouched.
fn csp_sources(lookup: Lookup<'_>, name: &'static str) -> Vec<String> {
    let Some(raw) = lookup(name) else {
        return Vec::new();
    };
    raw.split([',', ' ', '\t', '\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            if is_valid_csp_host_source(s) {
                Some(s.to_string())
            } else {
                tracing::warn!(
                    setting = name,
                    value = s,
                    "not a CSP host source (need a scheme and a host, no path, no quote); dropping it"
                );
                None
            }
        })
        .collect()
}

/// The Content-Security-Policy to send, resolved once at startup.
///
/// Both inputs used to be read from the environment on **every response**, and
/// the `report-uri` variant rebuilt and re-parsed the whole policy string each
/// time. Resolving once turns serving a response into a name lookup and a
/// `HeaderValue` clone, and — the reason this type exists — makes the policy an
/// input a caller can set instead of a process-global a test has to mutate.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    /// `content-security-policy`, or the `-report-only` variant when
    /// `CSP_REPORT_ONLY` is truthy.
    csp_header_name: &'static str,
    /// The assembled policy, including `report-uri` when one is configured.
    csp: HeaderValue,
}

impl SecurityHeaders {
    /// Resolve the security headers from a settings lookup.
    ///
    /// - `CSP_REPORT_ONLY` (truthy) — send the policy as report-only, so a
    ///   tightening can be observed before it starts blocking.
    /// - `CSP_REPORT_URI` — appended to the policy as `report-uri`.
    /// - `CSP_FRAME_SRC` — host sources (see [`csp_sources`]) appended to a new
    ///   `frame-src 'self' ...` directive, so a site can allow embedding
    ///   specific frames (YouTube, Captivate, HubSpot) without widening
    ///   `default-src`. Absent or empty, the policy carries no `frame-src` at
    ///   all, exactly as before: every frame is blocked by `default-src 'self'`.
    /// - `CSP_IMG_SRC` — host sources appended to the existing `img-src 'self'
    ///   data:` directive, for a thumbnail or avatar an embed's page builder
    ///   component pulls from the embed's own host.
    ///
    /// Nothing configured sends the exact `DEFAULT_CSP` bytes: the three
    /// settings only ever append to or extend a directive, never replace one,
    /// so an unconfigured site's policy is unchanged.
    ///
    /// A `report-uri` that cannot go in a header value (a control character, a
    /// newline) is dropped with a warning rather than silently discarding the
    /// whole policy, which is what the per-response version did: its
    /// `HeaderValue::from_str` failure left the response with **no CSP at all**.
    pub(crate) fn from_lookup(lookup: Lookup<'_>) -> Self {
        let csp_header_name = if crate::config::parse_bool_or(lookup, "CSP_REPORT_ONLY", false) {
            CSP_REPORT_ONLY
        } else {
            CSP_ENFORCE
        };

        let frame_sources = csp_sources(lookup, "CSP_FRAME_SRC");
        let img_sources = csp_sources(lookup, "CSP_IMG_SRC");
        let widened = !frame_sources.is_empty() || !img_sources.is_empty();

        let mut policy = DEFAULT_CSP.to_str().unwrap_or_default().to_string();
        if !frame_sources.is_empty() {
            policy = format!("{policy}; frame-src 'self' {}", frame_sources.join(" "));
        }
        if !img_sources.is_empty() {
            policy = policy.replacen(
                "img-src 'self' data:;",
                &format!("img-src 'self' data: {};", img_sources.join(" ")),
                1,
            );
        }

        let csp = match lookup("CSP_REPORT_URI") {
            Some(report_uri) if !report_uri.trim().is_empty() => {
                let with_report = format!("{policy}; report-uri {}", report_uri.trim());
                HeaderValue::from_str(&with_report).unwrap_or_else(|_| {
                    tracing::warn!(
                        report_uri = %report_uri,
                        "CSP_REPORT_URI is not a valid header value; serving the policy without it"
                    );
                    if widened {
                        HeaderValue::from_str(&policy).unwrap_or_else(|_| DEFAULT_CSP.clone())
                    } else {
                        DEFAULT_CSP.clone()
                    }
                })
            }
            _ if widened => {
                HeaderValue::from_str(&policy).unwrap_or_else(|_| DEFAULT_CSP.clone())
            }
            _ => DEFAULT_CSP.clone(),
        };

        Self {
            csp_header_name,
            csp,
        }
    }
}

impl Default for SecurityHeaders {
    /// The enforcing default policy with no `report-uri`.
    fn default() -> Self {
        Self {
            csp_header_name: CSP_ENFORCE,
            csp: DEFAULT_CSP.clone(),
        }
    }
}

/// Inject security response headers on every request.
///
/// Headers set:
/// - `Content-Security-Policy` (or `Content-Security-Policy-Report-Only`)
/// - `X-Frame-Options: DENY`
/// - `X-Content-Type-Options: nosniff`
/// - `Referrer-Policy: strict-origin-when-cross-origin`, unless the route set
///   `no-referrer`
/// - `Permissions-Policy: camera=(), microphone=(), geolocation=()`
/// - `Strict-Transport-Security`, only when the request arrived over HTTPS, which
///   is known from `X-Forwarded-Proto` sent by a trusted proxy
///   ([`forwarded_https`]); the header from any other peer is ignored
///
/// The CSP comes from [`SecurityHeaders`] on the application state, resolved at
/// startup. script-src does not include 'unsafe-inline', so an inline script is
/// blocked (see `DEFAULT_CSP`). style-src keeps 'unsafe-inline' for inline
/// `style=` attributes (low XSS risk).
pub async fn inject_security_headers(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // `X-Forwarded-Proto` only from a trusted proxy, the gate
    // `X-Forwarded-For` goes through (see `client_addr`).
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let is_https = forwarded_https(
        peer,
        request.headers(),
        state.rate_limiter().trusted_proxies(),
    );

    let mut response = next.run(request).await;
    apply(
        response.headers_mut(),
        &state.runtime().security_headers,
        is_https,
    );
    response
}

/// Write the security headers onto a response.
///
/// A pure core so the header set is testable without a router or an `AppState`:
/// the middleware above is a thin wrapper that decides `is_https` and hands the
/// response headers over.
pub(crate) fn apply(headers: &mut HeaderMap, security: &SecurityHeaders, is_https: bool) {
    headers.insert(security.csp_header_name, security.csp.clone());
    headers.insert("x-frame-options", DENY.clone());
    headers.insert("x-content-type-options", NOSNIFF.clone());
    // A page whose URL carries a secret (a reset or unsubscribe token) sends
    // `no-referrer` so no link on it leaks the token; overwriting that with the
    // site default would undo it. Only the strictest policy survives: any
    // other value a route set is replaced by the default.
    if headers.get("referrer-policy") != Some(&NO_REFERRER) {
        headers.insert("referrer-policy", REFERRER.clone());
    }
    headers.insert("permissions-policy", PERMISSIONS.clone());

    // HSTS — only on HTTPS connections. Sending it over plain HTTP would ask a
    // browser to pin a scheme the deployment may not serve.
    if is_https {
        headers.insert("strict-transport-security", HSTS.clone());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn nosniff_header_value() {
        assert_eq!(NOSNIFF.to_str().unwrap(), "nosniff");
    }

    #[test]
    fn x_frame_options_value() {
        assert_eq!(DENY.to_str().unwrap(), "DENY");
    }

    #[test]
    fn referrer_policy_value() {
        assert_eq!(
            REFERRER.to_str().unwrap(),
            "strict-origin-when-cross-origin"
        );
    }

    #[test]
    fn permissions_policy_value() {
        assert_eq!(
            PERMISSIONS.to_str().unwrap(),
            "camera=(), microphone=(), geolocation=()"
        );
    }

    #[test]
    fn hsts_value() {
        assert_eq!(
            HSTS.to_str().unwrap(),
            "max-age=31536000; includeSubDomains"
        );
    }

    #[test]
    fn default_csp_includes_self() {
        let csp = DEFAULT_CSP.to_str().unwrap();
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
    }

    /// Resolve `SecurityHeaders` from an explicit settings map — no process
    /// environment involved, which is the point of the `from_lookup` split.
    fn from_map(pairs: &[(&str, &str)]) -> SecurityHeaders {
        let settings: std::collections::HashMap<&str, &str> = pairs.iter().copied().collect();
        SecurityHeaders::from_lookup(&|name| settings.get(name).map(|v| (*v).to_string()))
    }

    #[test]
    fn nothing_configured_enforces_the_default_policy() {
        let headers = from_map(&[]);
        assert_eq!(headers.csp_header_name, CSP_ENFORCE);
        assert_eq!(headers.csp, DEFAULT_CSP);
        // The `Default` impl has to agree with "nothing configured", or a caller
        // building state without a config would get a different policy.
        let default = SecurityHeaders::default();
        assert_eq!(default.csp_header_name, headers.csp_header_name);
        assert_eq!(default.csp, headers.csp);
    }

    #[test]
    fn report_only_switches_the_header_name_not_the_policy() {
        for truthy in ["1", "true", "TRUE", "yes", "on"] {
            let headers = from_map(&[("CSP_REPORT_ONLY", truthy)]);
            assert_eq!(
                headers.csp_header_name, CSP_REPORT_ONLY,
                "{truthy:?} should report only"
            );
            assert_eq!(headers.csp, DEFAULT_CSP, "the policy itself is unchanged");
        }
        for falsy in ["0", "false", "off", ""] {
            assert_eq!(
                from_map(&[("CSP_REPORT_ONLY", falsy)]).csp_header_name,
                CSP_ENFORCE,
                "{falsy:?} should enforce"
            );
        }
    }

    #[test]
    fn report_uri_is_appended_to_the_policy() {
        let headers = from_map(&[("CSP_REPORT_URI", "https://csp.example.com/report")]);
        let csp = headers.csp.to_str().unwrap();
        assert!(csp.starts_with(DEFAULT_CSP.to_str().unwrap()));
        assert!(csp.ends_with("; report-uri https://csp.example.com/report"));
        // Still enforcing: the two settings are independent.
        assert_eq!(headers.csp_header_name, CSP_ENFORCE);
    }

    /// An unusable `report-uri` must cost the report endpoint, not the policy.
    ///
    /// The per-response version failed open here: `HeaderValue::from_str`
    /// returned `Err` for a value with a control character and the whole
    /// `headers.insert` was skipped, so the response carried **no CSP at all**.
    #[test]
    fn an_unusable_report_uri_keeps_the_policy() {
        for bad in ["https://example.com/\nX-Evil: 1", "\u{7f}", "   "] {
            let headers = from_map(&[("CSP_REPORT_URI", bad)]);
            assert_eq!(
                headers.csp, DEFAULT_CSP,
                "{bad:?} must fall back to the full default policy"
            );
        }
    }

    /// The full header set, asserted against a bare `HeaderMap` — no router and
    /// no `AppState`, because `apply` is where the decision lives.
    #[test]
    fn every_security_header_is_written() {
        let mut headers = HeaderMap::new();
        apply(&mut headers, &SecurityHeaders::default(), false);

        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            headers.get("referrer-policy").unwrap(),
            "strict-origin-when-cross-origin"
        );
        assert_eq!(
            headers.get("permissions-policy").unwrap(),
            "camera=(), microphone=(), geolocation=()"
        );
        assert_eq!(
            headers.get(CSP_ENFORCE).unwrap(),
            DEFAULT_CSP.to_str().unwrap()
        );
        // No HSTS without HTTPS.
        assert!(headers.get("strict-transport-security").is_none());
    }

    #[test]
    fn hsts_is_written_only_over_https() {
        let mut headers = HeaderMap::new();
        apply(&mut headers, &SecurityHeaders::default(), true);
        assert_eq!(
            headers.get("strict-transport-security").unwrap(),
            "max-age=31536000; includeSubDomains"
        );
    }

    /// Report-only mode changes which header carries the policy, and must not
    /// leave an enforcing one behind alongside it.
    #[test]
    fn report_only_mode_writes_only_the_report_only_header() {
        let mut headers = HeaderMap::new();
        apply(
            &mut headers,
            &from_map(&[("CSP_REPORT_ONLY", "true")]),
            false,
        );
        assert!(headers.get(CSP_REPORT_ONLY).is_some());
        assert!(
            headers.get(CSP_ENFORCE).is_none(),
            "an enforcing policy alongside the report-only one would block anyway"
        );
    }

    // Whether `X-Forwarded-Proto` earns HSTS is decided by
    // `client_addr::forwarded_https` (unit tested there) and exercised through
    // the real middleware, with a trusted and an untrusted socket peer, in
    // `tests/forwarded_proto_hsts_test.rs`.

    // --- CSP_FRAME_SRC / CSP_IMG_SRC (K7) ---

    #[test]
    fn valid_csp_host_sources() {
        for ok in [
            "https://www.youtube-nocookie.com",
            "https://player.captivate.fm",
            "https://*.hubspot.net",
            "http://example.com",
        ] {
            assert!(is_valid_csp_host_source(ok), "{ok}");
        }
    }

    #[test]
    fn invalid_csp_host_sources() {
        for bad in [
            "*",
            "'unsafe-inline'",
            "data:",
            "https://example.com/path",
            "https://example.com/",
            "https://example.com?x=1",
            "https://example.com#f",
            "https://example.com'",
            "https://*",
            "example.com",
            "javascript:alert(1)",
        ] {
            assert!(!is_valid_csp_host_source(bad), "{bad}");
        }
    }

    #[test]
    fn nothing_configured_sends_the_policy_byte_for_byte_unchanged() {
        let headers = from_map(&[]);
        assert_eq!(headers.csp, DEFAULT_CSP);
    }

    #[test]
    fn configured_frame_sources_are_appended_with_self() {
        let headers = from_map(&[(
            "CSP_FRAME_SRC",
            "https://www.youtube-nocookie.com, https://player.captivate.fm",
        )]);
        let csp = headers.csp.to_str().unwrap();
        assert!(
            csp.contains("frame-src 'self' https://www.youtube-nocookie.com https://player.captivate.fm"),
            "{csp}"
        );
        // Every other directive is exactly as the default sends it.
        assert!(csp.contains("img-src 'self' data:;"), "{csp}");
        assert!(csp.starts_with(DEFAULT_CSP.to_str().unwrap()), "{csp}");
    }

    #[test]
    fn configured_img_sources_extend_the_existing_directive() {
        let headers = from_map(&[("CSP_IMG_SRC", "https://*.hubspot.net")]);
        let csp = headers.csp.to_str().unwrap();
        assert!(
            csp.contains("img-src 'self' data: https://*.hubspot.net;"),
            "{csp}"
        );
        assert!(!csp.contains("frame-src"), "{csp}");
    }

    #[test]
    fn an_invalid_csp_source_is_dropped_and_the_rest_kept() {
        let headers = from_map(&[(
            "CSP_FRAME_SRC",
            "https://www.youtube-nocookie.com, *, 'unsafe-inline', data:",
        )]);
        let csp = headers.csp.to_str().unwrap();
        assert!(
            csp.contains("frame-src 'self' https://www.youtube-nocookie.com"),
            "{csp}"
        );
        assert!(!csp.contains(" * "), "{csp}");
        assert!(!csp.contains("unsafe-inline"), "{csp}");
    }

    #[test]
    fn only_invalid_csp_sources_leaves_the_directive_untouched() {
        let headers = from_map(&[("CSP_FRAME_SRC", "*"), ("CSP_IMG_SRC", "'unsafe-inline'")]);
        assert_eq!(headers.csp, DEFAULT_CSP);
    }

    #[test]
    fn csp_sources_and_report_uri_compose() {
        let headers = from_map(&[
            ("CSP_FRAME_SRC", "https://www.youtube-nocookie.com"),
            ("CSP_REPORT_URI", "https://csp.example.com/report"),
        ]);
        let csp = headers.csp.to_str().unwrap();
        assert!(csp.contains("frame-src 'self' https://www.youtube-nocookie.com"));
        assert!(csp.ends_with("; report-uri https://csp.example.com/report"), "{csp}");
    }
}
