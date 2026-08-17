//! Security response headers middleware.
//!
//! Injects Content-Security-Policy, X-Frame-Options, HSTS, and other
//! security headers on all responses. CSP prevents XSS even when
//! sanitization is bypassed. Other headers protect against clickjacking,
//! MIME sniffing, and downgrade attacks.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Request},
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

/// Static header values — constructed at compile time from string literals.
static DENY: HeaderValue = HeaderValue::from_static("DENY");
static NOSNIFF: HeaderValue = HeaderValue::from_static("nosniff");
static REFERRER: HeaderValue = HeaderValue::from_static("strict-origin-when-cross-origin");
static PERMISSIONS: HeaderValue =
    HeaderValue::from_static("camera=(), microphone=(), geolocation=()");
static HSTS: HeaderValue = HeaderValue::from_static("max-age=31536000; includeSubDomains");

/// The default CSP policy.
///
/// All inline scripts have been externalized to static JS files.
/// Template-dependent data is passed via `<script type="application/json">`
/// data blocks (which are not subject to CSP because they don't execute).
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
    ///
    /// A `report-uri` that cannot go in a header value (a control character, a
    /// newline) is dropped with a warning rather than silently discarding the
    /// whole policy, which is what the per-response version did: its
    /// `HeaderValue::from_str` failure left the response with **no CSP at all**.
    pub(crate) fn from_lookup(lookup: crate::config::Lookup<'_>) -> Self {
        let csp_header_name = if crate::config::parse_bool_or(lookup, "CSP_REPORT_ONLY", false) {
            CSP_REPORT_ONLY
        } else {
            CSP_ENFORCE
        };

        let csp = match lookup("CSP_REPORT_URI") {
            Some(report_uri) if !report_uri.trim().is_empty() => {
                let policy = format!(
                    "{}; report-uri {}",
                    DEFAULT_CSP.to_str().unwrap_or_default(),
                    report_uri.trim()
                );
                HeaderValue::from_str(&policy).unwrap_or_else(|_| {
                    tracing::warn!(
                        report_uri = %report_uri,
                        "CSP_REPORT_URI is not a valid header value; serving the policy without it"
                    );
                    DEFAULT_CSP.clone()
                })
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
/// - `Referrer-Policy: strict-origin-when-cross-origin`
/// - `Permissions-Policy: camera=(), microphone=(), geolocation=()`
/// - `Strict-Transport-Security` (only when request arrived via HTTPS)
///
/// The CSP comes from [`SecurityHeaders`] on the application state, resolved at
/// startup. script-src does not include 'unsafe-inline' — all inline scripts have
/// been externalized to static JS files. style-src keeps 'unsafe-inline' for
/// inline `style=` attributes (low XSS risk).
pub async fn inject_security_headers(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let is_https = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|proto| proto == "https");

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
    headers.insert("referrer-policy", REFERRER.clone());
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

    /// The middleware is layered with state in `main.rs`; the routing wrapper
    /// itself carries no logic beyond reading `x-forwarded-proto`.
    #[test]
    fn forwarded_proto_decides_https() {
        for (value, expected) in [
            (Some("https"), true),
            (Some("http"), false),
            (Some("HTTPS"), false),
            (None, false),
        ] {
            let mut builder = Request::builder().uri("/");
            if let Some(value) = value {
                builder = builder.header("x-forwarded-proto", value);
            }
            let request = builder.body(Body::empty()).unwrap();
            let is_https = request
                .headers()
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|proto| proto == "https");
            assert_eq!(is_https, expected, "x-forwarded-proto: {value:?}");
        }
    }
}
