#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Running the site from the admin (B161): clear all caches, run cron, and
//! the status report.
//!
//! Before B161 a cache clear was `trovato cache clear` on the command line
//! only, cron ran from the cron report for `administer site` alone, and there
//! was no status report. Each action here is a POST through the forms
//! pipeline (no token, no action) behind its own permission.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::cache::generation::{self, CacheKind};

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    body: String,
) -> axum::response::Response {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The token and build id of the form whose `action` is `action`.
fn form_pairs(html: &str, action: &str) -> String {
    let at = html
        .find(&format!(r#"action="{action}""#))
        .unwrap_or_else(|| panic!("a form posting to {action}"));
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    let scoped = &scoped[..scoped.find("</form>").unwrap()];
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap()),
        urlencoding::encode(&input_value(scoped, "_form_build_id").unwrap()),
    )
}

/// Delete the account named `name` when the test ends.
fn forget_user(app: &TestApp, name: String) {
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&name)
            .execute(&db)
            .await;
    });
}

async fn admin_cookies(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    forget_user(app, name.clone());
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// A signed-in non-administrator holding exactly `permissions`.
async fn holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> String {
    let name = common::username(prefix);
    forget_user(app, name.clone());
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    if !permissions.is_empty() {
        common::grant_via_role(app, id, permissions).await;
    }
    app.login(&name, "test-password-123").await
}

fn assert_refused(status: StatusCode, who: &str, what: &str) {
    assert!(
        status == StatusCode::FORBIDDEN
            || status == StatusCode::UNAUTHORIZED
            || status.is_redirection(),
        "{what} must be refused to {who}, got {status}"
    );
}

/// The three pages refuse an anonymous visitor and a signed-in user holding
/// none of their permissions, GET and POST, and the POSTs need the form's
/// token even from an administrator.
#[test]
fn the_operations_refuse_anyone_without_their_permission() {
    run_test(async {
        let app = shared_app().await;
        let plain = holding(app, "b161plain", &["access administration pages"]).await;

        for (who, cookies) in [("anonymous", ""), ("a plain user", plain.as_str())] {
            for uri in [
                "/admin/config/performance",
                "/admin/reports/status",
                "/admin/reports/cron",
            ] {
                let (status, _) = get(app, cookies, uri).await;
                assert_refused(status, who, &format!("GET {uri}"));
            }
            for uri in ["/admin/config/performance", "/admin/reports/cron"] {
                let response = post_form(app, cookies, uri, "op=Go".into()).await;
                assert_refused(response.status(), who, &format!("POST {uri}"));
            }
        }
        let (status, _) = get(app, &plain, "/admin/config/performance").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "signed in: a 403");

        let admin = admin_cookies(app, "b161csrf").await;
        let response = post_form(app, &admin, "/admin/config/performance", "op=Go".into()).await;
        assert!(
            !response.status().is_redirection(),
            "no token, no clear: got {}",
            response.status()
        );
    });
}

/// **The finding (K-cache-clear-ui).** The performance page has a Clear all
/// caches button; pressing it bumps every cache kind's generation, so every
/// server drops its copies, and the page says "Caches cleared.". `clear
/// caches` alone is enough.
#[test]
fn clear_all_caches_clears_every_kind_and_says_so() {
    run_test(async {
        let app = shared_app().await;
        let cookies = holding(app, "b161cache", &["clear caches"]).await;

        let (status, html) = get(app, &cookies, "/admin/config/performance").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Clear all caches"), "the button");

        let before: Vec<i64> = generations(app).await;
        let body = form_pairs(&html, "/admin/config/performance");
        let response = post_form(app, &cookies, "/admin/config/performance", body).await;
        assert!(
            response.status().is_redirection(),
            "a clear redirects back, got {}",
            response.status()
        );
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/admin/config/performance"
        );
        let after: Vec<i64> = generations(app).await;
        for ((kind, b), a) in CacheKind::ALL.iter().zip(&before).zip(&after) {
            assert!(a > b, "{} was not bumped ({b} -> {a})", kind.as_str());
        }

        let (status, html) = get(app, &cookies, "/admin/config/performance").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("Caches cleared."),
            "the message after a clear"
        );
    });
}

/// Every kind's generation counter, in [`CacheKind::ALL`] order.
async fn generations(app: &TestApp) -> Vec<i64> {
    let mut out = Vec::new();
    for kind in CacheKind::ALL {
        out.push(
            generation::current(app.state.redis(), kind)
                .await
                .unwrap_or(0),
        );
    }
    out
}

/// **The finding (K-cron-ui).** The status report lists the versions, the
/// database, the cache server, cron (with a link to run it manually), the
/// file system and the security settings, and shows no secret.
#[test]
fn the_status_report_checks_the_site_and_shows_no_secret() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b161status").await;

        let (status, html) = get(app, &cookies, "/admin/reports/status").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "Status report",
            "Trovato",
            env!("CARGO_PKG_VERSION"),
            "PostgreSQL",
            "Database schema",
            "Redis",
            "Cron maintenance tasks",
            "run cron manually",
            "File system",
            "Session cookies",
            "Updates",
            "Maintenance mode",
        ] {
            assert!(html.contains(needle), "the report shows {needle:?}");
        }
        // Not one connection string, key or password.
        for var in ["DATABASE_URL", "REDIS_URL", "CRON_KEY"] {
            if let Ok(value) = std::env::var(var)
                && value.len() > 3
            {
                assert!(!html.contains(&value), "the report leaks {var}");
            }
        }
        assert!(!html.contains("postgres://"), "no connection string");
        assert!(!html.contains("redis://"), "no connection string");
    });
}

/// `run cron` alone opens the cron report and runs cron; it does not open
/// the status report, which stays with `administer site`.
#[test]
fn run_cron_alone_runs_cron() {
    run_test(async {
        let app = shared_app().await;
        let cookies = holding(app, "b161cron", &["run cron"]).await;

        let (status, html) = get(app, &cookies, "/admin/reports/cron").await;
        assert_eq!(status, StatusCode::OK);
        let body = form_pairs(&html, "/admin/reports/cron");
        let response = post_form(app, &cookies, "/admin/reports/cron", body).await;
        assert!(
            response.status().is_redirection(),
            "run cron redirects back, got {}",
            response.status()
        );
        let (_, html) = get(app, &cookies, "/admin/reports/cron").await;
        assert!(
            html.contains("Cron ran in") || html.contains("already running"),
            "the flash says what Run now did"
        );

        let (status, _) = get(app, &cookies, "/admin/reports/status").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    });
}

/// **The finding (K-admin-pages).** A group's bare address (`/admin/config`,
/// `/admin/structure`, `/admin/extend`, `/admin/reports`) was a 404; it now
/// leads to the overview that lists the group's pages, and the overview
/// lists the new pages to the administrator.
#[test]
fn a_group_address_leads_to_its_overview() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b161groups").await;

        for slug in ["config", "structure", "extend", "reports"] {
            let response = app
                .request_with_cookies(
                    Request::get(format!("/admin/{slug}"))
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::PERMANENT_REDIRECT,
                "/admin/{slug}"
            );
            assert_eq!(
                response.headers().get(header::LOCATION).unwrap(),
                &format!("/admin/{slug}/overview")
            );
        }

        let (status, html) = get(app, &cookies, "/admin/reports/overview").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("/admin/reports/status"),
            "Status report listed"
        );
        let (status, html) = get(app, &cookies, "/admin/config/overview").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("/admin/config/performance"),
            "Performance listed"
        );
    });
}
