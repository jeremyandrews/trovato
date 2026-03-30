//! Security response headers middleware.
//!
//! Injects Content-Security-Policy, X-Frame-Options, HSTS, and other
//! security headers on all responses. CSP prevents XSS even when
//! sanitization is bypassed. Other headers protect against clickjacking,
//! MIME sniffing, and downgrade attacks.

use axum::{
    body::Body,
    http::{HeaderValue, Request},
    middleware::Next,
    response::Response,
};

/// Static header values — constructed at compile time from string literals.
static DENY: HeaderValue = HeaderValue::from_static("DENY");
static NOSNIFF: HeaderValue = HeaderValue::from_static("nosniff");
static REFERRER: HeaderValue = HeaderValue::from_static("strict-origin-when-cross-origin");
static PERMISSIONS: HeaderValue =
    HeaderValue::from_static("camera=(), microphone=(), geolocation=()");
static HSTS: HeaderValue = HeaderValue::from_static("max-age=31536000; includeSubDomains");

/// The default CSP policy (no report-uri variant).
static DEFAULT_CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'self'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data:; \
     font-src 'self'; \
     connect-src 'self'; \
     frame-ancestors 'none'",
);

/// Inject security response headers on every request.
///
/// Headers set:
/// - `Content-Security-Policy` (or `Content-Security-Policy-Report-Only`)
/// - `X-Frame-Options: DENY`
/// - `X-Content-Type-Options: nosniff`
/// - `Referrer-Policy: strict-origin-when-cross-origin`
/// - `Permissions-Policy: camera=(), microphone=(), geolocation=()`
/// - `Strict-Transport-Security` (only when request arrived via HTTPS)
pub async fn inject_security_headers(request: Request<Body>, next: Next) -> Response {
    let is_https = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|proto| proto == "https");

    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    // Content-Security-Policy
    // style-src includes 'unsafe-inline' because base.html has inline <style>.
    // This is tracked as tech debt — extracting styles to a static file
    // would allow removing 'unsafe-inline'.
    let csp_report_only = std::env::var("CSP_REPORT_ONLY")
        .ok()
        .is_some_and(|v| v == "true" || v == "1");

    let csp_header_name = if csp_report_only {
        "content-security-policy-report-only"
    } else {
        "content-security-policy"
    };

    // Use the default CSP or build a custom one with report-uri
    if let Ok(report_uri) = std::env::var("CSP_REPORT_URI") {
        let mut csp = DEFAULT_CSP.to_str().unwrap_or("").to_string();
        csp.push_str("; report-uri ");
        csp.push_str(&report_uri);
        if let Ok(val) = HeaderValue::from_str(&csp) {
            headers.insert(csp_header_name, val);
        }
    } else {
        headers.insert(csp_header_name, DEFAULT_CSP.clone());
    }

    headers.insert("x-frame-options", DENY.clone());
    headers.insert("x-content-type-options", NOSNIFF.clone());
    headers.insert("referrer-policy", REFERRER.clone());
    headers.insert("permissions-policy", PERMISSIONS.clone());

    // HSTS — only on HTTPS connections
    if is_https {
        headers.insert("strict-transport-security", HSTS.clone());
    }

    response
}
