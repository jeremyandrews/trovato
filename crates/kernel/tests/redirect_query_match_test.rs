#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B164: a redirect's source can carry a query string.
//!
//! Before this, `check_redirect` matched `request.uri().path()` alone, so
//! `/article.php?sid=1` and `/article.php?sid=2` — two different pages on a
//! legacy site — could not redirect to two different destinations: whichever
//! redirect existed for `/article.php` (if any) answered every query. A
//! redirect whose source names a query now matches only a request carrying
//! that exact query (its parameters normalized: sorted, percent-encoding
//! canonical, so order and encoding don't matter); a request whose query
//! doesn't match any stored one falls back to a path-only redirect exactly as
//! before.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use common::{TestApp, defer_cleanup, run_test, shared_app};
use tower::ServiceExt;
use trovato_kernel::services::redirect::Redirect;
use uuid::Uuid;

/// A redirect from `source` to `destination`, removed when the test ends.
async fn redirect(app: &TestApp, source: &str, destination: &str) {
    let created = Redirect::create(&app.db, source, destination, 301, "")
        .await
        .expect("create redirect");
    let pool = app.db.clone();
    let id = created.id;
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM redirect WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

/// The status and `Location` of `uri` through the redirect middleware, over a
/// fallback that reports "no redirect" (and never the request's query, so a
/// test here cannot mistake the fallback's own behavior for the
/// middleware's).
async fn follow(app: &TestApp, uri: &str) -> (StatusCode, Option<String>) {
    let router = Router::new().fallback(|| async { "no redirect" }).layer(
        axum::middleware::from_fn_with_state(
            app.state.clone(),
            trovato_kernel::middleware::check_redirect,
        ),
    );
    let response = router
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (response.status(), location)
}

/// Like [`follow`], but the fallback echoes the query string it received (as
/// a header), so a test can check that a request which matches no redirect
/// reaches the rest of the app with its query untouched.
async fn follow_observing_query(app: &TestApp, uri: &str) -> (StatusCode, Option<String>) {
    let router = Router::new()
        .fallback(|request: Request<Body>| async move {
            let query = request.uri().query().unwrap_or("").to_string();
            ([("x-observed-query", query)]).into_response()
        })
        .layer(axum::middleware::from_fn_with_state(
            app.state.clone(),
            trovato_kernel::middleware::check_redirect,
        ));
    let response = router
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let observed = response
        .headers()
        .get("x-observed-query")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (status, observed)
}

/// Every test in this file needs the plugin enabled and a fresh app; this
/// macro is the shared setup each `#[test]` starts with.
macro_rules! redirects_app {
    () => {{
        shared_app()
            .await
            .ensure_plugin_enabled("trovato_redirects")
            .await;
        &TestApp::new().await
    }};
}

#[test]
fn two_query_sources_match_their_own_request() {
    run_test(async {
        let app = redirects_app!();
        common::ensure_plugin_tables(app, "trovato_redirects");
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];

        redirect(app, &format!("/article-{s}.php?sid=1"), "/a").await;
        redirect(app, &format!("/article-{s}.php?sid=2"), "/b").await;

        let (status, location) = follow(app, &format!("/article-{s}.php?sid=1")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location.as_deref(), Some("/a"));

        let (status, location) = follow(app, &format!("/article-{s}.php?sid=2")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location.as_deref(), Some("/b"));
    });
}

#[test]
fn an_unmatched_query_falls_back_to_the_path_only_redirect() {
    run_test(async {
        let app = redirects_app!();
        common::ensure_plugin_tables(app, "trovato_redirects");
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];
        let path = format!("/article-{s}.php");

        redirect(app, &format!("{path}?sid=1"), "/a").await;
        redirect(app, &path, "/c").await;

        // No exact match for sid=3, so the path-only redirect answers, and
        // (B160, unchanged) the query goes with it.
        let (status, location) = follow(app, &format!("{path}?sid=3")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location.as_deref(), Some("/c?sid=3"));
    });
}

#[test]
fn an_unmatched_query_with_no_path_only_redirect_is_not_redirected() {
    run_test(async {
        let app = redirects_app!();
        common::ensure_plugin_tables(app, "trovato_redirects");
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];
        let path = format!("/article-{s}.php");

        redirect(app, &format!("{path}?sid=1"), "/a").await;

        // sid=3 names no redirect, and neither does the bare path: through.
        let (status, _) = follow(app, &format!("{path}?sid=3")).await;
        assert_eq!(status, StatusCode::OK);
    });
}

#[test]
fn a_path_only_redirect_still_matches_any_query() {
    run_test(async {
        let app = redirects_app!();
        common::ensure_plugin_tables(app, "trovato_redirects");
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];
        let path = format!("/old-{s}");
        let new = format!("/new-{s}");

        redirect(app, &path, &new).await;

        // Byte for byte as on main: whatever query, the path-only redirect
        // answers and keeps the query (B160).
        let (status, location) = follow(app, &format!("{path}?anything=goes&x=1")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location, Some(format!("{new}?anything=goes&x=1")));
    });
}

#[test]
fn query_parameter_order_does_not_matter() {
    run_test(async {
        let app = redirects_app!();
        common::ensure_plugin_tables(app, "trovato_redirects");
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];
        let path = format!("/search-{s}");

        redirect(app, &format!("{path}?b=2&a=1"), "/sorted").await;

        let (status, location) = follow(app, &format!("{path}?a=1&b=2")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location.as_deref(), Some("/sorted"));
    });
}

#[test]
fn percent_encoding_is_normalized() {
    run_test(async {
        let app = redirects_app!();
        common::ensure_plugin_tables(app, "trovato_redirects");
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];
        let path = format!("/search-{s}");

        // Stored with a literal space.
        redirect(app, &format!("{path}?q=a b"), "/found").await;

        // Requested with the space percent-encoded, and again as a `+`
        // (form-encoded): both normalize to the same stored source.
        let (status, location) = follow(app, &format!("{path}?q=a%20b")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location.as_deref(), Some("/found"));

        let (status, location) = follow(app, &format!("{path}?q=a+b")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location.as_deref(), Some("/found"));
    });
}

#[test]
fn an_unmatched_request_s_query_reaches_the_app_untouched() {
    run_test(async {
        let app = redirects_app!();
        common::ensure_plugin_tables(app, "trovato_redirects");
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];

        // No redirect at all names this path: the request passes through,
        // and its query is exactly what was sent — nothing added.
        let (status, observed) =
            follow_observing_query(app, &format!("/no-such-path-{s}?a=1&b=2")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(observed.as_deref(), Some("a=1&b=2"));
    });
}
