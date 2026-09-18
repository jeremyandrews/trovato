#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The login page, as an anonymous visitor loads it.
//!
//! `templates/user/login.html` linked "Forgot password?" to
//! `/user/password-reset`, which the kernel registers for POST only: it is the
//! JSON endpoint that mints a reset token, not a page. A person who clicked the
//! link got a 405. The page a person can load is `/user/recover`.
//!
//! The test follows the link the page actually renders rather than asserting on
//! template text, so it fails if the href drifts to anything that does not load.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};

/// Its own client address, so this file does not spend the shared rate-limit
/// bucket that requests without `X-Forwarded-For` fall into.
const CLIENT_IP: &str = "10.71.0.1";

async fn get(app: &TestApp, path: &str) -> (StatusCode, String) {
    let response = app
        .request(
            Request::get(path)
                .header("x-forwarded-for", CLIENT_IP)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Every `href` in `html` that is a same-origin absolute path.
fn internal_links(html: &str) -> Vec<String> {
    html.split("href=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter(|href| href.starts_with('/') && !href.starts_with("//"))
        .map(str::to_string)
        .collect()
}

/// Render the anonymous login page.
async fn login_page(app: &TestApp) -> String {
    let (status, html) = get(app, "/user/login").await;
    assert_eq!(status, StatusCode::OK, "GET /user/login");
    html
}

/// "Forgot password?" leads to the recovery page, which a browser can load.
#[test]
fn the_forgot_password_link_points_at_a_page_a_person_can_load() {
    run_test(async {
        let app = shared_app().await;
        let html = login_page(app).await;

        let forgot = html
            .split("<a ")
            .find(|anchor| anchor.contains("Forgot password?"))
            .expect("the login page renders a Forgot password link");
        let href = internal_links(forgot)
            .into_iter()
            .next()
            .expect("the Forgot password link has an internal href");

        assert_eq!(href, "/user/recover");
        let (status, _) = get(app, &href).await;
        assert_eq!(status, StatusCode::OK, "GET {href}");
    });
}
