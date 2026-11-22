#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Maintenance mode (B107).
//!
//! Its own binary with one test: the mode is a site-wide setting, and while it
//! is on every other test in the same binary would get a 503.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use trovato_kernel::middleware::maintenance::{MAINTENANCE_MESSAGE_KEY, MAINTENANCE_MODE_KEY};
use trovato_kernel::models::SiteConfig;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> axum::response::Response {
    app.request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await
}

async fn post_form(app: &TestApp, cookies: &str, uri: &str, body: String) -> StatusCode {
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
    .status()
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

async fn mode_on(app: &TestApp) -> bool {
    SiteConfig::get(&app.db, MAINTENANCE_MODE_KEY)
        .await
        .unwrap()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[test]
fn maintenance_mode_turns_the_site_away_except_for_admins_and_sign_in() {
    run_test(async {
        let app = shared_app().await;
        let db = app.db.clone();
        common::defer_cleanup(move || async move {
            SiteConfig::set(&db, MAINTENANCE_MODE_KEY, serde_json::json!(false))
                .await
                .ok();
            SiteConfig::set(&db, MAINTENANCE_MESSAGE_KEY, serde_json::json!(""))
                .await
                .ok();
        });

        let admin_name = common::username("b107maint");
        let admin = app
            .create_and_login_admin(
                &admin_name,
                "correct-horse-battery-staple",
                &format!("{admin_name}@test.local"),
            )
            .await;
        let (_, plain) = user_holding(app, "b107mplain", &["access content"]).await;

        // The toggle is on the site settings screen, and refuses anyone
        // without `administer site`.
        let response = get(app, &admin, "/admin/config/site").await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_string(response).await;
        let pairs = form_pairs(&html, "/admin/config/site/maintenance");
        for (who, cookies) in [("anonymous", ""), ("a plain user", plain.as_str())] {
            let status = post_form(
                app,
                cookies,
                "/admin/config/site/maintenance",
                format!("{pairs}&maintenance_mode=1&maintenance_message=x"),
            )
            .await;
            assert!(
                status == StatusCode::FORBIDDEN
                    || status == StatusCode::UNAUTHORIZED
                    || status.is_redirection(),
                "{who} may not turn maintenance on, got {status}"
            );
            assert!(!mode_on(app).await, "{who} did not turn it on");
        }

        // Without the form's token, nothing changes.
        let status = post_form(
            app,
            &admin,
            "/admin/config/site/maintenance",
            "maintenance_mode=1".to_string(),
        )
        .await;
        assert!(!status.is_redirection());
        assert!(!mode_on(app).await);

        let status = post_form(
            app,
            &admin,
            "/admin/config/site/maintenance",
            format!("{pairs}&maintenance_mode=1&maintenance_message=Back+at+noon."),
        )
        .await;
        assert!(status.is_redirection(), "saved, got {status}");
        assert!(mode_on(app).await, "the administrator turned it on");

        // **The finding.** Anonymous: a 503, the themed page, the message, and
        // Retry-After.
        let response = get(app, "", "/").await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(response.headers().contains_key(header::RETRY_AFTER));
        let html = body_string(response).await;
        assert!(html.contains("Back at noon."));
        assert!(html.contains("maintenance-page"), "the themed template");

        // A signed-in user without the bypass is turned away too.
        let response = get(app, &plain, "/").await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // An administrator holds the bypass and uses the site.
        let response = get(app, &admin, "/admin").await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = get(app, &admin, "/").await;
        assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // What a site needs to come back still serves.
        let response = get(app, "", "/user/login").await;
        assert_eq!(response.status(), StatusCode::OK, "sign in");
        let response = get(app, "", "/health").await;
        assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "health");
        let response = get(app, "", "/static/css/theme.css").await;
        assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "static");

        // And off again.
        let response = get(app, &admin, "/admin/config/site").await;
        let html = body_string(response).await;
        let pairs = form_pairs(&html, "/admin/config/site/maintenance");
        let status = post_form(
            app,
            &admin,
            "/admin/config/site/maintenance",
            format!("{pairs}&maintenance_message="),
        )
        .await;
        assert!(status.is_redirection());
        assert!(!mode_on(app).await);
        let response = get(app, "", "/").await;
        assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    });
}
