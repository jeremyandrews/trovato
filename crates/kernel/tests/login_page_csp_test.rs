#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The login page's JavaScript under the kernel's own Content-Security-Policy.
//!
//! The kernel sends `script-src 'self' ...` with no `'unsafe-inline'`, nonce or
//! hash, so a browser refuses to run an inline `<script>` block. The passkey
//! sign-in code in `templates/user/login.html` was exactly such a block: on a
//! default install it never ran, and the "Sign in with a passkey" button, which
//! starts hidden and is revealed by that script, never appeared.
//!
//! The test reads the page the way a browser applies the policy: from the header
//! the response actually carries and the markup it actually renders. Every
//! executable `<script>` on the page must load from a same-origin file, and the
//! passkey script must be one of them and be served.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};

/// Its own client address, so this file does not spend the shared rate-limit
/// bucket that requests without `X-Forwarded-For` fall into.
const CLIENT_IP: &str = "10.72.0.1";

const PASSKEY_SCRIPT: &str = "/static/js/passkey-login.js";

async fn get(app: &TestApp, path: &str) -> axum::response::Response {
    app.request(
        Request::get(path)
            .header("x-forwarded-for", CLIENT_IP)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Every `<script ...>` opening tag in `html`, as source text.
fn script_tags(html: &str) -> Vec<String> {
    html.match_indices("<script")
        .filter_map(|(start, _)| {
            let rest = &html[start..];
            rest.find('>').map(|end| rest[..=end].to_string())
        })
        .collect()
}

/// The value of `name="..."` in a tag, if present.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    tag[start..].split('"').next().map(str::to_string)
}

/// The directive `name` of a CSP policy, e.g. `script-src 'self' ...`.
fn directive<'a>(policy: &'a str, name: &str) -> Option<&'a str> {
    policy
        .split(';')
        .map(str::trim)
        .find(|d| d.split_whitespace().next() == Some(name))
}

#[test]
fn the_login_page_runs_no_script_its_own_csp_blocks() {
    run_test(async {
        let app = shared_app().await;
        let response = get(app, "/user/login").await;
        assert_eq!(response.status(), StatusCode::OK, "GET /user/login");

        let policy = response
            .headers()
            .get("content-security-policy")
            .expect("the login page is served with an enforcing CSP")
            .to_str()
            .unwrap()
            .to_string();
        let script_src =
            directive(&policy, "script-src").expect("the policy has a script-src directive");
        // The premise of the test. If this changes, inline scripts would run and
        // the rest of the assertions stop describing what a browser does.
        assert!(
            !script_src.contains("'unsafe-inline'")
                && !script_src.contains("'nonce-")
                && !script_src.contains("'sha"),
            "script-src must not allow inline scripts: {script_src}"
        );

        let html = text_of(response).await;
        let tags = script_tags(&html);
        for tag in &tags {
            let is_data_block = attribute(tag, "type").as_deref() == Some("application/json");
            let src = attribute(tag, "src");
            assert!(
                is_data_block || src.as_deref().is_some_and(|s| s.starts_with('/')),
                "the login page has an inline script its CSP blocks: {tag}"
            );
        }

        assert!(
            tags.iter()
                .any(|tag| attribute(tag, "src").as_deref() == Some(PASSKEY_SCRIPT)),
            "the login page loads {PASSKEY_SCRIPT}; script tags were {tags:?}"
        );
    });
}

#[test]
fn the_passkey_sign_in_script_is_served_as_javascript() {
    run_test(async {
        let app = shared_app().await;
        let response = get(app, PASSKEY_SCRIPT).await;
        assert_eq!(response.status(), StatusCode::OK, "GET {PASSKEY_SCRIPT}");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/javascript")
        );

        let body = text_of(response).await;
        for endpoint in ["/user/webauthn/login/start", "/user/webauthn/login/finish"] {
            assert!(
                body.contains(endpoint),
                "the passkey script drives {endpoint}"
            );
        }
        // The element ids it reveals and reads must match the template.
        for id in [
            "passkey-login",
            "passkey-login-button",
            "passkey-login-message",
        ] {
            assert!(body.contains(&format!("\"{id}\"")), "script uses #{id}");
        }
    });
}
