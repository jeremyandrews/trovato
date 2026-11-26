#![allow(clippy::unwrap_used, clippy::expect_used)]
//! One HTML error page.
//!
//! A browser (an `Accept` naming `text/html`) is shown every error as the
//! site's themed `error.html` inside the normal page wrapper, with a link to
//! `/` or, on an admin path, `/admin`, never to `javascript:`. Nobody signed in
//! is sent to the login form. Any other caller gets the `AppError` body.
//!
//! Before this there were five pages: an empty-body 404 for an unknown path,
//! JSON for a missing `/item/{id}` and for the item forms, unthemed HTML from
//! the gather pages and the four error helpers (whose "Go back" link was a
//! `javascript:` URL the CSP blocks), and plain text for a disabled
//! registration form.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};

const BROWSER: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const MISSING: &str = "00000000-0000-0000-0000-00000000b843";

struct Answer {
    status: StatusCode,
    content_type: String,
    location: Option<String>,
    body: String,
}

fn header_of(headers: &header::HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn get(app: &TestApp, path: &str, accept: Option<&str>, cookies: &str) -> Answer {
    let mut builder = Request::get(path).header("x-forwarded-for", test_ip_for("b84-page"));
    if let Some(accept) = accept {
        builder = builder.header(header::ACCEPT, accept);
    }
    let response = app
        .request_with_cookies(builder.body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let content_type = header_of(response.headers(), header::CONTENT_TYPE).unwrap_or_default();
    let location = header_of(response.headers(), header::LOCATION);
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Answer {
        status,
        content_type,
        location,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// The themed page: inside the site wrapper, the error template, a home link
/// for the area, and no `javascript:` link.
fn assert_themed(answer: &Answer, status: StatusCode, home: &str, what: &str) {
    assert_eq!(answer.status, status, "{what}: {}", answer.body);
    assert!(
        answer.content_type.starts_with("text/html"),
        "{what}: {}",
        answer.content_type
    );
    assert!(
        answer
            .body
            .contains(&format!("error-page--{}", status.as_u16())),
        "{what}: not the error template: {}",
        answer.body
    );
    assert!(
        answer.body.contains("<html") && answer.body.contains("</nav>"),
        "{what}: not inside the page wrapper: {}",
        answer.body
    );
    assert!(
        answer.body.contains(&format!(r#"<a href="{home}">"#)),
        "{what}: no link to {home}: {}",
        answer.body
    );
    assert!(
        !answer.body.contains("javascript:"),
        "{what}: a javascript: link: {}",
        answer.body
    );
}

fn assert_app_error(answer: &Answer, status: StatusCode, what: &str) {
    assert_eq!(answer.status, status, "{what}: {}", answer.body);
    let value: serde_json::Value = serde_json::from_str(&answer.body)
        .unwrap_or_else(|_| panic!("{what}: not JSON: {}", answer.body));
    assert!(value["code"].is_string(), "{what}: {}", answer.body);
    assert!(value["message"].is_string(), "{what}: {}", answer.body);
}

#[test]
fn a_browser_gets_one_themed_page_for_every_error() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b84-page", &[]).await;

        // The unknown path that was an empty 404.
        let a = get(app, "/b84-no-such-page", Some(BROWSER), "").await;
        assert_themed(&a, StatusCode::NOT_FOUND, "/", "unknown path");
        assert!(
            a.body.contains("error-page__search"),
            "the 404 suggestion: {}",
            a.body
        );

        // An unknown admin path links to the admin.
        let a = get(app, "/admin/b84-no-such-screen", Some(BROWSER), &cookies).await;
        assert_themed(&a, StatusCode::NOT_FOUND, "/admin", "unknown admin path");
        assert!(
            !a.body.contains("error-page__search"),
            "no site search in the admin: {}",
            a.body
        );

        // A missing item, which was JSON.
        let a = get(app, &format!("/item/{MISSING}"), Some(BROWSER), "").await;
        assert_themed(&a, StatusCode::NOT_FOUND, "/", "missing item");

        // The item forms, which were JSON 403 and 404.
        let a = get(app, "/item/add/page", Some(BROWSER), &cookies).await;
        assert_themed(&a, StatusCode::FORBIDDEN, "/", "add form, unpermitted");
        let a = get(
            app,
            &format!("/item/{MISSING}/edit"),
            Some(BROWSER),
            &cookies,
        )
        .await;
        assert_themed(&a, StatusCode::NOT_FOUND, "/", "edit form, missing item");

        // A gather page, which was unthemed.
        let a = get(app, "/gather/b84_no_such_gather", Some(BROWSER), "").await;
        assert_themed(&a, StatusCode::NOT_FOUND, "/", "missing gather");

        // An admin screen the user may not open: the helper's 403, themed.
        let a = get(app, "/admin/structure/types", Some(BROWSER), &cookies).await;
        assert_themed(
            &a,
            StatusCode::FORBIDDEN,
            "/admin",
            "admin screen, unpermitted",
        );
    });
}

#[test]
fn nobody_signed_in_is_shown_access_denied_with_the_login_form() {
    run_test(async {
        let app = shared_app().await;
        // The item form refuses an anonymous caller with 401, which a browser
        // is shown as the 403 access-denied page with the login form on it.
        let a = get(app, "/item/add/page", Some(BROWSER), "").await;
        assert_themed(&a, StatusCode::FORBIDDEN, "/", "add form, nobody");
        assert!(a.location.is_none(), "{:?}", a.location);
        assert!(a.body.contains(r#"action="/user/login""#), "{}", a.body);
        // A script is told 401.
        let a = get(app, "/item/add/page", None, "").await;
        assert_app_error(&a, StatusCode::UNAUTHORIZED, "add form, script");
    });
}

#[test]
fn any_other_caller_gets_app_error() {
    run_test(async {
        let app = shared_app().await;
        let a = get(app, "/b84-no-such-page", None, "").await;
        assert_app_error(&a, StatusCode::NOT_FOUND, "unknown path");
        let a = get(app, "/b84-no-such-page", Some("application/json"), "").await;
        assert_app_error(&a, StatusCode::NOT_FOUND, "unknown path, JSON");
        let a = get(app, &format!("/item/{MISSING}"), Some("*/*"), "").await;
        assert_app_error(&a, StatusCode::NOT_FOUND, "missing item");
    });
}

/// Axum's own refusals (here a JSON body that does not parse) are plain text;
/// they take the same two forms.
#[test]
fn a_framework_rejection_is_app_error_too() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_content_locking").await;
        let response = app
            .request(
                Request::post("/api/lock/heartbeat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", test_ip_for("b84-page"))
                    .body(Body::from("{not json"))
                    .unwrap(),
            )
            .await;
        let status = response.status();
        assert!(status.is_client_error(), "{status}");
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&bytes)));
        assert!(value["code"].is_string(), "{value}");
        assert!(value["message"].is_string(), "{value}");
    });
}
