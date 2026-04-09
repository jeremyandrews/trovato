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
/// The default CSP policy.
///
/// `script-src` includes `'unsafe-inline'` because several pages inject
/// configuration via inline `<script>` blocks (Scolta search config,
/// Editor.js initialization, Trovato AJAX init). A future improvement
/// would use nonce-based CSP to avoid unsafe-inline.
static DEFAULT_CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'self'; \
     script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval' https://cdn.jsdelivr.net; \
     style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
     img-src 'self' data:; \
     font-src 'self' https://fonts.gstatic.com; \
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

    #[tokio::test]
    async fn middleware_sets_security_headers() {
        use axum::{Router, routing::get};
        use tower::ServiceExt;

        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(inject_security_headers));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(response.headers().get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            response.headers().get("referrer-policy").unwrap(),
            "strict-origin-when-cross-origin"
        );
        assert!(response.headers().get("content-security-policy").is_some());
        // No HSTS without HTTPS
        assert!(
            response
                .headers()
                .get("strict-transport-security")
                .is_none()
        );
    }

    #[tokio::test]
    async fn middleware_sets_hsts_on_https() {
        use axum::{Router, routing::get};
        use tower::ServiceExt;

        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(inject_security_headers));

        let request = Request::builder()
            .uri("/test")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.headers().get("strict-transport-security").unwrap(),
            "max-age=31536000; includeSubDomains"
        );
    }
}
