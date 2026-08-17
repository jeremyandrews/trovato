#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Navigation landmarks and current-page markers in rendered pages.
//!
//! `templates/page.html` had four `<nav>` elements and not one `aria-label`: the
//! site nav, two breadcrumbs and the footer nav. A screen reader lists landmarks
//! by label, so several unlabelled navigations on one page force the user to
//! enter each one to work out which is which. The active main-menu link, in turn,
//! was marked only by the CSS class `site-nav__link--active`, which assistive
//! technology cannot see.
//!
//! The generic test here is the valuable one: every `<nav>` on every page this
//! suite renders must carry a label, so a future template cannot quietly
//! reintroduce the defect.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateMenuLink, MenuLink};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE_SEED_LOCK: i64 = 0x_F406_0000_0001;
const ITEM_TYPE: &str = "nav_landmark_test";

fn admin_ctx() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_item_type(app: &TestApp) {
    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Nav Landmark Test', 'Fixture type', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .execute(&mut *tx)
    .await
    .expect("seed item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .create(
            ITEM_TYPE,
            "Nav Landmark Test",
            Some("Fixture type"),
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();
}

async fn create_item(app: &TestApp) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: format!("Landmarks {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("nav landmark test".to_string()),
            },
            &admin_ctx(),
        )
        .await
        .expect("create item")
        .id
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn get(app: &TestApp, cookies: &str, path: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, text_of(response).await)
}

/// Every `<nav>` opening tag in `html`, as source text.
fn nav_tags(html: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<nav") {
        let after = &rest[start..];
        // `<nav` must be the whole element name, not the prefix of another.
        let next = after[4..].chars().next().unwrap_or(' ');
        if next == '>' || next.is_whitespace() {
            let end = after.find('>').unwrap_or(after.len() - 1);
            tags.push(after[..=end].to_string());
        }
        rest = &rest[start + 4..];
    }
    tags
}

/// An admin session, which is needed to render the admin pages.
async fn admin_session(app: &TestApp) -> String {
    let name = format!("navadmin_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

/// The general rule: no page this suite can render may contain an unlabelled
/// navigation landmark.
#[test]
fn every_nav_landmark_on_every_rendered_page_is_labelled() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;
        let cookies = admin_session(app).await;

        let paths = [
            "/".to_string(),
            format!("/item/{item_id}"),
            "/search?q=landmark".to_string(),
            "/admin".to_string(),
            "/admin/content".to_string(),
            "/admin/content/files".to_string(),
            "/admin/media".to_string(),
            "/admin/structure/aliases".to_string(),
            "/admin/structure/categories".to_string(),
        ];

        let mut checked = 0;
        for path in &paths {
            let (status, html) = get(app, &cookies, path).await;
            assert_eq!(status, StatusCode::OK, "GET {path}");

            let tags = nav_tags(&html);
            assert!(
                !tags.is_empty(),
                "{path} rendered no nav landmarks at all, which makes this test vacuous"
            );

            for tag in tags {
                assert!(
                    tag.contains("aria-label") || tag.contains("aria-labelledby"),
                    "unlabelled navigation landmark on {path}: {tag}"
                );
                checked += 1;
            }
        }

        assert!(
            checked >= 10,
            "expected to check a meaningful number of landmarks, saw {checked}"
        );
    });
}

/// The active main-menu link is announced as the current page, not merely styled
/// as one.
#[test]
fn the_active_main_menu_link_is_marked_as_the_current_page() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;
        let item_path = format!("/item/{item_id}");

        // A main-menu link pointing at the page we are about to load, so
        // `current_path` matches it.
        let link = MenuLink::create(
            &app.db,
            CreateMenuLink {
                menu_name: Some("main".to_string()),
                path: item_path.clone(),
                title: format!("Landmark Link {}", Uuid::now_v7().simple()),
                parent_id: None,
                weight: Some(0),
                hidden: Some(false),
                plugin: Some("core".to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create menu link");

        let (status, html) = get(app, "", &item_path).await;
        assert_eq!(status, StatusCode::OK);

        assert!(
            html.contains("site-nav__link--active"),
            "test setup: the menu link must be rendered as active, page was:\n{html}"
        );
        assert!(
            html.contains("aria-current=\"page\""),
            "the active link must announce itself as the current page"
        );

        // And the marker is on the active link itself, not somewhere else on the
        // page: the active class and the attribute travel together.
        let active_at = html.find("site-nav__link--active").unwrap();
        let tag_end = html[active_at..].find('>').unwrap() + active_at;
        assert!(
            html[active_at..tag_end].contains("aria-current=\"page\""),
            "aria-current must be on the active link's own tag, tag was:\n{}",
            &html[active_at..tag_end]
        );

        sqlx::query("DELETE FROM menu_link WHERE id = $1")
            .bind(link.id)
            .execute(&app.db)
            .await
            .expect("clean up menu link");
    });
}

/// A breadcrumb trail ends at the current page, and says so.
#[test]
fn the_last_breadcrumb_is_marked_as_the_current_page() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;

        let (status, html) = get(app, "", &format!("/item/{item_id}")).await;
        assert_eq!(status, StatusCode::OK);

        assert!(
            html.contains("aria-label=\"Breadcrumb\""),
            "the breadcrumb nav must be labelled as one, page was:\n{html}"
        );

        let crumb_at = html
            .find("aria-label=\"Breadcrumb\"")
            .expect("a breadcrumb nav");
        let crumb_end = html[crumb_at..].find("</nav>").expect("a closed nav") + crumb_at;
        assert!(
            html[crumb_at..crumb_end].contains("aria-current=\"page\""),
            "the trailing crumb is the current page and must say so, trail was:\n{}",
            &html[crumb_at..crumb_end]
        );
    });
}

/// The admin sidebar marks where you are, too. Its links carried only a CSS
/// class.
#[test]
fn the_admin_sidebar_marks_the_current_screen() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);

        let active_at = html
            .find("class=\"active\"")
            .expect("the dashboard link must render as active on /admin");
        let tag_start = html[..active_at].rfind('<').expect("an opening tag");
        let tag_end = html[active_at..].find('>').expect("a tag end") + active_at;

        assert!(
            html[tag_start..tag_end].contains("aria-current=\"page\""),
            "the active admin link must announce itself, tag was:\n{}",
            &html[tag_start..tag_end]
        );
    });
}
