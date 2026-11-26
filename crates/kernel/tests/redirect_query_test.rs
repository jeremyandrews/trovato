#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B160: old links through the redirect middleware.
//!
//! A Drupal site without clean URLs published `/?q=node/96`; on Trovato that
//! reached the front page. And a redirect dropped the query string, so
//! `/old?page=2` landed on `/new`. Both now reach the page the link meant.
//!
//! The middleware runs here over a stub router, layered as `main.rs` layers
//! it; the shared test app's router does not carry it.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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

/// The status and `Location` of `uri` through the redirect middleware.
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

#[test]
fn old_links_keep_their_query_and_drupal_q_urls_redirect() {
    run_test(async {
        // An app built after the redirects plugin is enabled: the redirect
        // cache exists only on a state built with it on.
        shared_app()
            .await
            .ensure_plugin_enabled("trovato_redirects")
            .await;
        let app = &TestApp::new().await;
        common::ensure_plugin_tables(app, "trovato_redirects");
        assert!(
            app.state.redirect_cache().is_some(),
            "the redirects plugin is enabled"
        );
        let s = Uuid::now_v7().simple().to_string();
        let s = &s[20..];
        let node = format!("/node/9{s}");
        let alias = format!("/blog/repairs-{s}");
        redirect(app, &node, &alias).await;
        let old = format!("/old-{s}");
        let new = format!("/new-{s}");
        redirect(app, &old, &new).await;

        // `/?q=node/96` follows `/node/96`'s redirect, in one hop.
        let (status, location) = follow(app, &format!("/?q={}", &node[1..])).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location.as_deref(), Some(alias.as_str()));

        // Its other parameters go with it.
        let (_, location) = follow(app, &format!("/?q={}&page=1", &node[1..])).await;
        assert_eq!(location, Some(format!("{alias}?page=1")));

        // A `q` path with no redirect of its own is sent to that path.
        let (status, location) = follow(app, &format!("/?q=blog-{s}")).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(location, Some(format!("/blog-{s}")));

        // A redirect keeps the query string.
        let (_, location) = follow(app, &format!("{old}?page=2")).await;
        assert_eq!(location, Some(format!("{new}?page=2")));

        // Not a way off the site.
        let (status, location) = follow(app, "/?q=//evil.example").await;
        assert_eq!(status, StatusCode::OK, "{location:?}");

        // The root without `q` is untouched.
        let (status, _) = follow(app, "/?page=1").await;
        assert_eq!(status, StatusCode::OK);
    });
}
