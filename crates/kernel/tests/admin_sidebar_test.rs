#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The admin sidebar offers only the screens the viewer may open (BL-117).
//!
//! `templates/page--admin.html` listed every administration screen to anyone
//! admitted to `/admin`, so a role delegated one job saw a column of links
//! that answered 403. These render the admin pages as such a role and follow
//! every sidebar link.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
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

/// The `href`s inside the admin sidebar.
fn sidebar_links(html: &str) -> Vec<String> {
    let start = html
        .find(r#"<nav class="admin-sidebar" id="admin-nav" aria-label="Administration">"#)
        .expect("the admin layout has its sidebar");
    let end = start + html[start..].find("</nav>").unwrap();
    html[start..end]
        .match_indices("href=\"")
        .map(|(at, _)| {
            let from = start + at + 6;
            html[from..from + html[from..].find('"').unwrap()].to_string()
        })
        .collect()
}

/// **The finding.** A role with `edit any content` and `access administration
/// pages` sees Content, and every link it is offered opens.
#[test]
fn a_content_editor_sees_content_and_no_link_that_answers_403() {
    run_test(async {
        let app = shared_app().await;
        let (_id, cookies) = common::user_holding(
            app,
            "b71side",
            &["access administration pages", "edit any content"],
        )
        .await;

        for page in ["/admin", "/admin/content"] {
            let (status, html) = get(app, &cookies, page).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            let links = sidebar_links(&html);
            assert!(
                links.iter().any(|l| l == "/admin/content"),
                "{page}: no Content link: {links:?}"
            );
            for absent in ["/admin/people", "/admin/plugins", "/admin/structure/types"] {
                assert!(
                    !links.iter().any(|l| l == absent),
                    "{page}: offers {absent}: {links:?}"
                );
            }
            for link in &links {
                let (status, _) = get(app, &cookies, link).await;
                assert_ne!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{page} links to {link}, which is 403"
                );
                assert!(
                    status.is_success(),
                    "{page} links to {link}, which answers {status}"
                );
            }
        }
    });
}

/// The administrator still sees the whole sidebar.
#[test]
fn the_administrator_sees_every_screen() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b71sideadm");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        let links = sidebar_links(&html);
        for expected in [
            "/admin/content",
            "/admin/structure/types",
            "/admin/people",
            "/admin/plugins",
            "/admin/config/site",
        ] {
            assert!(
                links.iter().any(|l| l == expected),
                "missing {expected}: {links:?}"
            );
        }
    });
}
