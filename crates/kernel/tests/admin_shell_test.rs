#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The admin shell: the sidebar's plugin screens, the dashboard's cards, the
//! comment settings address, and the admin stylesheet under a site theme.
//!
//! The sidebar tested plugin names no plugin has (`"comments"` and
//! `"categories"`), so it hid both screens from everyone; the dashboard linked
//! three areas; the comment settings address answered 405 to a GET; and the
//! admin styles were inline in templates a site theme replaces.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b107shell"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        String::from_utf8_lossy(&bytes).replace("&#x2F;", "/"),
    )
}

/// The `href`s between `open` and the next `close` in `html`.
fn hrefs_in(html: &str, open: &str, close: &str) -> Vec<String> {
    let start = html.find(open).expect("the region is on the page");
    let end = start + html[start..].find(close).unwrap();
    html[start..end]
        .match_indices("href=\"")
        .map(|(at, _)| {
            let from = start + at + 6;
            html[from..from + html[from..].find('"').unwrap()].to_string()
        })
        .collect()
}

fn sidebar_links(html: &str) -> Vec<String> {
    hrefs_in(
        html,
        r#"<nav class="admin-sidebar" id="admin-nav" aria-label="Administration">"#,
        "</nav>",
    )
}

fn dashboard_links(html: &str) -> Vec<String> {
    hrefs_in(html, r#"<div class="dashboard-grid">"#, "</main>")
}

async fn admin_cookies(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// **The finding.** With the comments and categories plugins enabled, the
/// sidebar links to both screens, and both open.
#[test]
fn the_sidebar_shows_comments_and_categories_when_their_plugins_are_enabled() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        app.ensure_plugin_enabled("trovato_categories").await;
        let cookies = admin_cookies(app, "b107side").await;

        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        let links = sidebar_links(&html);
        for expected in ["/admin/content/comments", "/admin/structure/categories"] {
            assert!(
                links.iter().any(|l| l == expected),
                "missing {expected}: {links:?}"
            );
            let (status, _) = get(app, &cookies, expected).await;
            assert_eq!(status, StatusCode::OK, "{expected}");
        }
    });
}

/// The dashboard offers one card per area, and only what the viewer may open:
/// every link it shows a delegated editor opens, and the areas the editor has
/// no permission for are not there.
#[test]
fn the_dashboard_lists_only_what_the_viewer_may_open() {
    run_test(async {
        let app = shared_app().await;
        let (_id, cookies) = common::user_holding(
            app,
            "b107dash",
            &["access administration pages", "edit any content"],
        )
        .await;
        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        let links = dashboard_links(&html);
        assert!(links.iter().any(|l| l == "/admin/content"), "{links:?}");
        for absent in [
            "/admin/people",
            "/admin/people/roles",
            "/admin/plugins",
            "/admin/structure/types",
            "/admin/structure/menus",
            "/admin/config/site",
            "/admin/gather",
        ] {
            assert!(!links.iter().any(|l| l == absent), "offers {absent}");
        }
        for link in links.iter().filter(|l| l.starts_with("/admin")) {
            let (status, _) = get(app, &cookies, link).await;
            assert!(status.is_success(), "dashboard links to {link}: {status}");
        }

        // The administrator gets a card for every area.
        app.ensure_plugin_enabled("trovato_comments").await;
        app.ensure_plugin_enabled("trovato_categories").await;
        let admin = admin_cookies(app, "b107dashadm").await;
        let (_, html) = get(app, &admin, "/admin").await;
        let links = dashboard_links(&html);
        for expected in [
            "/admin/content",
            "/admin/content/comments",
            "/admin/people",
            "/admin/people/roles",
            "/admin/structure/types",
            "/admin/structure/categories",
            "/admin/structure/menus",
            "/admin/structure/aliases",
            "/admin/gather",
            "/admin/structure/tiles",
            "/admin/plugins",
            "/admin/config/site",
            "/admin/reports/search",
        ] {
            assert!(
                links.iter().any(|l| l == expected),
                "missing {expected}: {links:?}"
            );
        }
    });
}

/// `GET /admin/content/comments/settings` serves the settings form; it was a
/// POST-only route and answered 405. Anonymous and plain users are refused.
#[test]
fn the_comment_settings_address_answers_a_get() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        let admin = admin_cookies(app, "b107cset").await;
        let (status, html) = get(app, &admin, "/admin/content/comments/settings").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(r#"id="comment-settings""#));
        assert!(html.contains(r#"action="/admin/content/comments/settings""#));

        let (status, _) = get(app, "", "/admin/content/comments/settings").await;
        assert!(
            status.is_redirection() || status == StatusCode::FORBIDDEN,
            "anonymous: {status}"
        );
        let (_id, plain) = common::user_holding(app, "b107csetplain", &[]).await;
        let (status, _) = get(app, &plain, "/admin/content/comments/settings").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "plain user");
    });
}

/// A site theme that replaces `base.html` with one of its own, which links
/// none of the kernel's stylesheets, still gets the admin stylesheet on an
/// admin page: it is linked from `page--admin.html`, not from `base.html`.
///
/// Since B199 `page--admin.html` is a whole document of its own, so the site
/// base no longer reaches the admin at all (before, it was used, and this
/// asserted so).
#[test]
fn an_admin_page_under_a_minimal_site_base_still_links_the_admin_stylesheet() {
    let site = std::env::temp_dir().join(format!("b107_site_theme_{}", std::process::id()));
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(
        site.join("base.html"),
        "<!DOCTYPE html><html><head><title>Site</title>{% block head %}{% endblock %}</head>\
         <body>{% block body %}{% endblock %}{% block scripts %}{% endblock %}</body></html>",
    )
    .unwrap();

    let engine = trovato_kernel::theme::ThemeEngine::new(
        &[common::project_root().join("templates"), site.clone()],
        None,
        std::sync::Arc::new(trovato_kernel::metrics::Metrics::new()),
    )
    .expect("the theme loads with a site base.html");
    let mut context = tera::Context::new();
    context.insert("content", "<p>admin</p>");
    let html = engine
        .tera()
        .render("page--admin.html", &context)
        .expect("page--admin renders");
    std::fs::remove_dir_all(&site).ok();

    assert!(
        !html.contains("<title>Site</title>"),
        "the site base does not draw the admin"
    );
    assert!(
        html.contains(r#"<link rel="stylesheet" href="/static/css/admin.css?v="#),
        "no admin stylesheet: {html}"
    );
    assert!(!html.contains("<style>"), "admin CSS is inline again");
}

/// The stylesheets are served. Before B199 the admin sheet imported the base
/// component sheet, for a theme that did not link it; since B199 the admin is
/// its own document whose sheet defines every component it draws (and the
/// names the shared form sheets read), so neither a theme nor base.css can
/// leave it unstyled, and base.css is no longer pulled in.
#[test]
fn the_admin_and_base_stylesheets_are_served() {
    run_test(async {
        let app = shared_app().await;
        let (status, css) = get(app, "", "/static/css/admin.css").await;
        assert_eq!(status, StatusCode::OK);
        assert!(css.contains(".admin-sidebar"));
        assert!(!css.contains("@import"), "the admin sheet stands alone");
        assert!(
            css.contains("--gray-300:"),
            "and defines what forms.css reads"
        );
        let (status, css) = get(app, "", "/static/css/base.css").await;
        assert_eq!(status, StatusCode::OK);
        assert!(css.contains("--gray-900"));

        // And the admin shell links the admin sheet, with nothing left inline.
        // Since B199 the shell is not drawn by base.html, so base.css is not
        // linked.
        let admin = admin_cookies(app, "b107css").await;
        let (_, html) = get(app, &admin, "/admin").await;
        assert!(!html.contains(r#"href="/static/css/base.css?v="#));
        assert!(html.contains(r#"href="/static/css/admin.css?v="#));
        assert!(!html.contains("<style>"), "inline CSS on the dashboard");
    });
}
