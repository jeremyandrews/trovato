#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Who sees the update banner, and what it says.
//!
//! Two claims worth a test rather than a comment. **Nobody but an administrator is
//! told the site's version**, which is a fingerprinting detail with no upside for a
//! visitor. And **the banner never fetches**: it reads what a cron check stored, so
//! no page render depends on GitHub being reachable.
//!
//! The banner has two levels, and which one appears comes from the release title,
//! so both are asserted on a rendered page rather than only on the parser.
//!
//! These tests write the shared fixture database's `update_status` key and restore
//! whatever was there, so they pass under default parallelism only because each
//! asserts on its own response body rather than on the key. Requires Postgres +
//! Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::update_status::UPDATE_STATUS_KEY;
use uuid::Uuid;

/// Serializes the tests that write the site-wide `update_status` key.
///
/// One key, one shared fixture database, and six tests that each need it to say
/// something different. A tokio mutex rather than a Postgres advisory lock because
/// this is the only test file that writes this key, and every test in a binary runs
/// on one runtime (`common::SHARED_RT`).
static BANNER_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// The rendered class attribute of an ordinary banner.
///
/// Asserted on the attribute rather than on the bare class name, because the page
/// carries a `<style>` block naming both classes whether a banner is rendered or
/// not — which is a way to write a test that can never fail.
const ORDINARY_BANNER: &str = r#"class="update-banner""#;

/// The rendered class attribute of a security banner.
const SECURITY_BANNER: &str = r#"class="update-banner update-banner--security""#;

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Store a status describing a release newer than anything this kernel is.
async fn store_status(app: &TestApp, version: &str, title: &str, is_security: bool) {
    SiteConfig::set(
        &app.db,
        UPDATE_STATUS_KEY,
        serde_json::json!({
            "latest_version": version,
            "latest_title": title,
            "is_security": is_security,
            "checked_at": 1_767_225_600i64,
        }),
    )
    .await
    .expect("store the update status");
}

async fn clear_status(app: &TestApp) {
    let _ = SiteConfig::delete(&app.db, UPDATE_STATUS_KEY).await;
}

async fn admin_dashboard(app: &TestApp) -> String {
    let name = format!("upadm_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let cookies = app.login(&name, "test-password-123").await;
    let response = app
        .request_with_cookies(
            Request::get("/admin").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_text(response).await
}

/// A newer ordinary release gets an ordinary banner, on the admin dashboard.
#[test]
fn an_admin_sees_an_ordinary_banner_for_an_ordinary_release() {
    run_test(async {
        let app = shared_app().await;
        let _guard = BANNER_LOCK.lock().await;
        store_status(app, "999.0.0", "999.0.0", false).await;

        let html = admin_dashboard(app).await;

        assert!(
            html.contains("Update available."),
            "the banner must appear, got: {html}"
        );
        assert!(html.contains("999.0.0"), "it must name the version");
        assert!(
            html.contains(ORDINARY_BANNER) && !html.contains(SECURITY_BANNER),
            "an ordinary release must not use the alarm styling"
        );
        assert!(
            html.contains(r#"role="status""#),
            "an ordinary banner is a status, not an alert"
        );

        clear_status(app).await;
    });
}

/// A security release gets the alarm.
#[test]
fn an_admin_sees_an_alarm_for_a_security_release() {
    run_test(async {
        let app = shared_app().await;
        let _guard = BANNER_LOCK.lock().await;
        store_status(app, "999.0.1", "[security] fixes CVE-2026-00000", true).await;

        let html = admin_dashboard(app).await;

        assert!(
            html.contains("Security release available."),
            "the security banner must appear, got: {html}"
        );
        assert!(
            html.contains(SECURITY_BANNER),
            "a security release must use the alarm styling"
        );
        assert!(
            html.contains(r#"role="alert""#),
            "a security banner is an alert, so assistive technology announces it"
        );

        clear_status(app).await;
    });
}

/// A stored status describing an older release raises nothing.
#[test]
fn no_banner_when_the_stored_release_is_older_than_the_running_one() {
    run_test(async {
        let app = shared_app().await;
        let _guard = BANNER_LOCK.lock().await;
        store_status(app, "0.0.1", "0.0.1", false).await;

        let html = admin_dashboard(app).await;

        assert!(
            !html.contains(ORDINARY_BANNER) && !html.contains(SECURITY_BANNER),
            "a site that is ahead of the stored release must see no banner"
        );

        clear_status(app).await;
    });
}

/// And nothing when no check has run.
#[test]
fn no_banner_before_any_check_has_run() {
    run_test(async {
        let app = shared_app().await;
        let _guard = BANNER_LOCK.lock().await;
        clear_status(app).await;

        let html = admin_dashboard(app).await;

        assert!(
            !html.contains(ORDINARY_BANNER) && !html.contains(SECURITY_BANNER),
            "an unchecked site must see no banner"
        );
    });
}

/// **Nobody but an administrator is told the version.** A visitor's page never
/// carries the banner or the version, whatever is stored.
#[test]
fn an_anonymous_visitor_is_never_told_the_version() {
    run_test(async {
        let app = shared_app().await;
        let _guard = BANNER_LOCK.lock().await;
        store_status(app, "999.0.1", "[security] fixes CVE-2026-00000", true).await;

        for path in ["/", "/user/login"] {
            let response = app
                .request(Request::get(path).body(Body::empty()).unwrap())
                .await;
            let html = body_text(response).await;
            assert!(
                !html.contains(ORDINARY_BANNER) && !html.contains(SECURITY_BANNER),
                "{path} must not carry the banner"
            );
            assert!(
                !html.contains("Security release available"),
                "{path} must not announce a security release to a visitor"
            );
            assert!(
                !html.contains("999.0.1"),
                "{path} must not name the latest version to a visitor"
            );
        }

        clear_status(app).await;
    });
}

/// A non-admin who is logged in is told no more than a visitor.
#[test]
fn an_authenticated_non_admin_is_never_told_the_version() {
    run_test(async {
        let app = shared_app().await;
        let _guard = BANNER_LOCK.lock().await;
        store_status(app, "999.0.1", "[security] fixes CVE-2026-00000", true).await;

        let name = format!("upusr_{}", Uuid::now_v7().simple());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;

        // The dashboard itself is refused, which is the first line.
        let response = app
            .request_with_cookies(
                Request::get("/admin").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // And the front page they can reach says nothing.
        let response = app
            .request_with_cookies(Request::get("/").body(Body::empty()).unwrap(), &cookies)
            .await;
        let html = body_text(response).await;
        assert!(!html.contains(ORDINARY_BANNER) && !html.contains(SECURITY_BANNER));
        assert!(!html.contains("999.0.1"));

        clear_status(app).await;
    });
}
