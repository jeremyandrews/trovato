#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The dashboard's panels, "Who can do this?", and the accessibility report.
//!
//! Before, the dashboard held only cards leading to each area: nothing said
//! what the viewer had written, what waited for them, whether cron worked
//! or what went wrong. No page said which roles may open a page, and
//! Reports had no accessibility report.
//!
//! Every row a test makes carries a fresh tag and is deleted when the test
//! ends. Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, test_ip_for};
use trovato_kernel::models::item::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";

fn tag() -> String {
    format!("db{}", Uuid::now_v7().simple())
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b199dashboard"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        String::from_utf8_lossy(&bytes)
            .replace("&#x2F;", "/")
            .replace("&amp;", "&")
            .replace("&#x3D;", "="),
    )
}

fn forget(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

/// An administrator's session and id; the account goes when the test ends.
async fn admin(app: &TestApp) -> (String, Uuid) {
    let name = common::username("dbadmin");
    let cookies = app
        .create_and_login_admin(&name, PASSWORD, &format!("{name}@test.local"))
        .await;
    let id = common::user_id_of(app, &name).await;
    forget(app, id);
    (cookies, id)
}

async fn holding(app: &TestApp, permissions: &[&str]) -> String {
    let (id, cookies) = common::user_holding(app, "dbuser", permissions).await;
    forget(app, id);
    cookies
}

/// An unpublished page by `author`, deleted when the test ends.
async fn item(app: &TestApp, title: &str, author: Uuid) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: title.to_string(),
                author_id: author,
                status: Some(0),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("dashboard test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

/// The panel with `key`, from its opening tag to its end.
fn panel<'a>(html: &'a str, key: &str) -> Option<&'a str> {
    let start = html.find(&format!(r#"aria-labelledby="dashboard-panel-{key}""#))?;
    let end = start + html[start..].find("</section>")?;
    Some(&html[start..end])
}

/// The dashboard says, in words, what the viewer wrote, what waits for
/// them, whether scheduled tasks work and which errors were logged; each
/// panel is a section labelled by its heading.
#[test]
fn the_dashboard_panels_say_what_needs_the_viewer() {
    run_test(async {
        let app = shared_app().await;
        // Comments to approve come from the comments plugin.
        common::ensure_plugin_tables(app, "trovato_comments");
        app.ensure_plugin_enabled("trovato_comments").await;
        let (cookies, id) = admin(app).await;
        let tag = tag();
        let item_id = item(app, &format!("Draft {tag}"), id).await;
        trovato_kernel::system_log::insert(
            &app.db,
            &[trovato_kernel::system_log::LogRecord {
                created: chrono::Utc::now().timestamp(),
                level: "error",
                log_type: "dashboard_test".to_string(),
                message: format!("Dashboard test failure {tag}"),
                context: Default::default(),
            }],
        )
        .await
        .unwrap();
        let pool = app.db.clone();
        let like = format!("%{tag}%");
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM system_log WHERE message LIKE $1")
                .bind(like)
                .execute(&pool)
                .await;
        });

        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        for (key, heading) in [
            ("content", "Your recent content"),
            ("waiting", "Waiting for you"),
            ("tasks", "Scheduled tasks"),
            ("errors", "Recent errors"),
        ] {
            let section = panel(&html, key).unwrap_or_else(|| panic!("the {key} panel: {html}"));
            assert!(
                section.contains(&format!(r#"<h2 id="dashboard-panel-{key}">{heading}</h2>"#)),
                "{section}"
            );
        }
        let content = panel(&html, "content").unwrap();
        assert!(
            content.contains("You have written 1 item; 1 not published."),
            "{content}"
        );
        assert!(content.contains(&format!(
            r#"href="/admin/content/{item_id}/edit">Draft {tag}</a>"#
        )));
        assert!(content.contains("page, not published, changed"));
        let errors = panel(&html, "errors").unwrap();
        assert!(
            errors.contains(&format!("Dashboard test failure {tag}")),
            "{errors}"
        );
        assert!(errors.contains("in the last 24 hours."));
        assert!(errors.contains(r#"href="/admin/reports/log?level=error""#));
        let waiting = panel(&html, "waiting").unwrap();
        assert!(waiting.contains("to review"), "{waiting}");
        assert!(waiting.contains("waiting for approval"));
        let tasks = panel(&html, "tasks").unwrap();
        assert!(tasks.contains(r#"href="/admin/reports/cron""#), "{tasks}");
    });
}

/// A panel is only for someone who may open the page it is about.
#[test]
fn a_dashboard_panel_is_shown_only_to_whoever_may_open_its_page() {
    run_test(async {
        let app = shared_app().await;
        let editor = holding(app, &["access administration pages", "edit any content"]).await;
        let (_, html) = get(app, &editor, "/admin").await;
        assert!(
            panel(&html, "waiting").is_some(),
            "drafts to review: {html}"
        );
        let waiting = panel(&html, "waiting").unwrap();
        assert!(
            !waiting.contains("waiting for approval"),
            "no comment moderation"
        );
        assert!(panel(&html, "tasks").is_none());
        assert!(panel(&html, "errors").is_none());
        assert!(!html.contains("/admin/reports/log?level=error"));

        // An author's own item links to its own edit form, not the admin one
        // they may not open.
        let (author, cookies) = common::user_holding(
            app,
            "dbauthor",
            &[
                "access administration pages",
                "create page content",
                "edit own content",
            ],
        )
        .await;
        forget(app, author);
        let tag = tag();
        let own = item(app, &format!("Mine {tag}"), author).await;
        let (_, html) = get(app, &cookies, "/admin").await;
        let content = panel(&html, "content").unwrap_or_else(|| panic!("{html}"));
        assert!(
            content.contains(&format!(r#"href="/item/{own}/edit">Mine {tag}</a>"#)),
            "{content}"
        );
        assert!(!content.contains("/admin/content/"));

        let visitor = holding(app, &["access administration pages"]).await;
        let (_, html) = get(app, &visitor, "/admin").await;
        for key in ["content", "waiting", "tasks", "errors"] {
            assert!(panel(&html, key).is_none(), "{key}");
        }
    });
}

/// Each page links to "Who can do this?" for someone who may change it, and
/// that page names the permission and the roles that hold it, recomputed
/// from the path.
#[test]
fn who_can_do_this_names_the_permission_and_the_roles_holding_it() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, _) = admin(app).await;
        let (_, html) = get(app, &cookies, "/admin/structure/menus").await;
        assert!(
            html.contains(r#"<a href="/admin/people/who-can?path=/admin/structure/menus">Who can do this?</a>"#),
            "{html}"
        );

        // A role holding the menus page's permission, with one member.
        let (member, _) = common::user_holding(app, "dbrole", &["administer site"]).await;
        forget(app, member);
        let roles = trovato_kernel::models::Role::get_user_roles(&app.db, member)
            .await
            .unwrap();
        let role = roles
            .iter()
            .find(|r| r.name.starts_with("testrole-"))
            .unwrap();

        let (status, html) = get(
            app,
            &cookies,
            "/admin/people/who-can?path=/admin/structure/menus",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Who can open Menus</h1>"), "{html}");
        assert!(html.contains("Menus needs the permission “administer site”."));
        assert!(html.contains(&format!(
            r#"<a href="/admin/people/roles/{}/edit">{}</a>"#,
            role.id, role.name
        )));
        assert!(html.contains("1 person"));

        // The page does not link to itself.
        assert!(!html.contains("who-can?path=/admin/people/who-can"));

        // A page only site administrators open.
        let (_, html) = get(app, &cookies, "/admin/people/who-can?path=/admin/plugins").await;
        assert!(
            html.contains("Only site administrators may open Plugins."),
            "{html}"
        );

        // A path that is not an admin page answers nothing about it.
        for bad in ["/user/1", "/admin/../user", "https://example.com/admin"] {
            let (status, html) = get(
                app,
                &cookies,
                &format!("/admin/people/who-can?path={}", urlencoding::encode(bad)),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                html.contains("That is not an administration page this site knows."),
                "{bad}"
            );
        }

        // Someone who may not change permissions sees neither.
        let editor = holding(app, &["access administration pages", "edit any content"]).await;
        let (_, html) = get(app, &editor, "/admin/content").await;
        assert!(!html.contains("/admin/people/who-can"));
        let (status, _) = get(app, &editor, "/admin/people/who-can?path=/admin/content").await;
        assert_ne!(status, StatusCode::OK);
    });
}

/// The accessibility report lists images without a text alternative, each
/// linking to where it is set, and is under Reports.
#[test]
fn the_accessibility_report_lists_images_without_a_text_alternative() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, id) = admin(app).await;
        let tag = tag();
        let file_id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) \
             VALUES ($1, $2, $3, $4, 'image/png', 10, 1, $5, $5)",
        )
        .bind(file_id)
        .bind(id)
        .bind(format!("{tag}.png"))
        .bind(format!("local://test/{tag}.png"))
        .bind(now)
        .execute(&app.db)
        .await
        .unwrap();
        let pool = app.db.clone();
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM file_managed WHERE id = $1")
                .bind(file_id)
                .execute(&pool)
                .await;
        });

        let (status, html) = get(app, &cookies, "/admin/reports/accessibility").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(r#"<h1 class="page-title">Accessibility</h1>"#),
            "{html}"
        );
        assert!(html.contains(&format!(
            r#"<a href="/admin/content/files/{file_id}">Add a text alternative to {tag}.png</a>"#
        )));
        assert!(html.contains("How this administration is checked"));
        let (_, landing) = get(app, &cookies, "/admin/reports/overview").await;
        assert!(landing.contains(r#"href="/admin/reports/accessibility""#));

        // Given a text alternative, the image leaves the list.
        sqlx::query("UPDATE file_managed SET alt_text = 'A test image' WHERE id = $1")
            .bind(file_id)
            .execute(&app.db)
            .await
            .unwrap();
        let (_, html) = get(app, &cookies, "/admin/reports/accessibility").await;
        assert!(!html.contains(&format!("{tag}.png")));

        let editor = holding(app, &["access administration pages", "edit any content"]).await;
        let (status, _) = get(app, &editor, "/admin/reports/accessibility").await;
        assert_ne!(status, StatusCode::OK);
    });
}
