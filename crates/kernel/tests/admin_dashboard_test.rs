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
        assert!(
            errors.contains("error in the last 24 hours</p>")
                || errors.contains("errors in the last 24 hours</p>"),
            "the status line's sentence: {errors}"
        );
        assert!(errors.contains(r#"href="/admin/reports/log?level=error""#));
        let waiting = panel(&html, "waiting").unwrap();
        assert!(waiting.contains("to review"), "{waiting}");
        assert!(
            waiting.contains("comment waiting for moderation</p>")
                || waiting.contains("comments waiting for moderation</p>"),
            "{waiting}"
        );
        assert!(waiting.contains(&format!(
            r#"href="/admin/content/comments?status={}""#,
            trovato_kernel::models::comment::CommentStatus::Pending.as_i16()
        )));
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
            !waiting.contains("waiting for moderation"),
            "no comment moderation"
        );
        assert!(
            !waiting.contains("/admin/content/comments"),
            "and no link to the comments"
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
        // The viewer is a site administrator, so the role's one member is
        // named (B252), not only counted.
        let member_name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(member)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(html.contains(&format!(
            r#"<a href="/admin/people/{member}/edit">{member_name}</a>"#
        )));

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

// --- status lines, compact cards and main actions (B245) ---

/// Seed one of every state a `Status` counts, each deleted when the test
/// ends: an unpublished item by `author` with a comment waiting for
/// moderation, a blocked account, and an error logged now.
async fn seed_every_status(app: &TestApp, author: Uuid) {
    common::ensure_plugin_tables(app, "trovato_comments");
    app.ensure_plugin_enabled("trovato_comments").await;
    let tag = tag();
    let item_id = item(app, &format!("Unpublished {tag}"), author).await;
    let now = chrono::Utc::now().timestamp();
    // Deleted with its item.
    sqlx::query(
        "INSERT INTO comment (id, item_id, author_id, body, status, created, changed) \
         VALUES ($1, $2, $3, $4, $5, $6, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(item_id)
    .bind(author)
    .bind(format!("Waiting {tag}"))
    .bind(trovato_kernel::models::comment::CommentStatus::Pending.as_i16())
    .bind(now)
    .execute(&app.db)
    .await
    .unwrap();
    let (blocked, _) = common::user_holding(app, "dbblocked", &["access content"]).await;
    forget(app, blocked);
    sqlx::query("UPDATE users SET status = 0 WHERE id = $1")
        .bind(blocked)
        .execute(&app.db)
        .await
        .unwrap();
    trovato_kernel::system_log::insert(
        &app.db,
        &[trovato_kernel::system_log::LogRecord {
            created: now,
            level: "error",
            log_type: "dashboard_test".to_string(),
            message: format!("Status test failure {tag}"),
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
}

/// The dashboard cards: each `<section>` in the card grid, by its heading's
/// text, tags removed.
fn cards(html: &str) -> Vec<(String, &str)> {
    let start = html
        .find(r#"<div class="dashboard-grid">"#)
        .expect("the card grid");
    let grid = &html[start..];
    grid.match_indices(r#"<section class="admin-card dashboard-card""#)
        .map(|(at, _)| {
            let section = &grid[at..at + grid[at..].find("</section>").unwrap()];
            let open = section.find("<h3").unwrap();
            let from = open + section[open..].find('>').unwrap() + 1;
            let to = from + section[from..].find("</h3>").unwrap();
            (text(&section[from..to]), section)
        })
        .collect()
}

/// `fragment` with its tags removed and its whitespace collapsed.
fn text(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in fragment.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of the filled button leading to `href`, if the page has one.
fn button_text(html: &str, href: &str) -> Option<String> {
    let open = format!(r#"<a href="{href}" class="button button--primary">"#);
    let from = html.find(&open)? + open.len();
    let to = from + html[from..].find("</a>")?;
    Some(text(&html[from..to]))
}

/// The registry's label for the main action at `href`.
fn primary_label(href: &str) -> &'static str {
    trovato_kernel::routes::admin_nav::LINKS
        .iter()
        .find(|l| l.href == href && l.primary)
        .map(|l| l.label)
        .unwrap_or_else(|| panic!("{href} is a main action in the registry"))
}

/// With one of every state seeded, the Dashboard says each count once: the
/// panels own comments and errors, the cards unpublished items and blocked
/// accounts, and cron is the Scheduled tasks panel's alone. A group card
/// lists none of its pages; it leads to its landing page and offers its main
/// action, as the registry names it.
#[test]
fn the_dashboard_says_each_count_once_and_its_cards_lead_to_landing_pages() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, id) = admin(app).await;
        seed_every_status(app, id).await;
        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);

        for sentence in [
            " unpublished item",
            " waiting for moderation",
            " blocked account",
            " in the last 24 hours",
        ] {
            assert_eq!(
                html.matches(sentence).count(),
                1,
                "{sentence:?} once on the Dashboard: {html}"
            );
        }
        assert!(!html.contains("Last run"), "cron is the tasks panel's");
        assert!(!html.contains("No run in the last 24 hours"));

        // The waiting comments are one element, the badge, inside its link.
        let badge = format!(
            r#"<a href="/admin/content/comments?status={}"><p class="status-line status-line--waiting">"#,
            trovato_kernel::models::comment::CommentStatus::Pending.as_i16()
        );
        assert!(html.contains(&badge), "{html}");
        let errors = panel(&html, "errors").unwrap();
        assert!(
            errors.contains(r#"<p class="status-line status-line--waiting">"#),
            "{errors}"
        );

        let cards = cards(&html);
        let titles: Vec<&str> = cards.iter().map(|(t, _)| t.as_str()).collect();
        for group in trovato_kernel::routes::admin_nav::GROUPS {
            let (_, card) = cards
                .iter()
                .find(|(t, _)| t == group.title)
                .unwrap_or_else(|| panic!("a {} card: {titles:?}", group.title));
            assert!(
                card.contains(&format!(
                    r#"<a href="{}">{}</a></h3>"#,
                    group.group.landing_href(),
                    group.title
                )),
                "{card}"
            );
            assert!(!card.contains("<ul"), "a group card lists no pages: {card}");
            assert!(card.contains(r#"<span class="icon-chip" aria-hidden="true"><svg"#));
        }
        let card = |title: &str| cards.iter().find(|(t, _)| t == title).unwrap().1;
        assert!(card("Content").contains(" unpublished item"));
        assert!(card("People").contains(" blocked account"));
        assert!(!card("Reports").contains("status-line"));
        for (title, href) in [
            ("Content", "/admin/content/add"),
            ("Structure", "/admin/structure/types/add"),
            ("People", "/admin/people/add"),
        ] {
            assert_eq!(
                button_text(card(title), href).as_deref(),
                Some(primary_label(href)),
                "{title}"
            );
        }
        for title in ["Appearance", "Extend", "Configuration", "Reports"] {
            assert!(!card(title).contains("button--primary"), "{title}");
        }
    });
}

/// A landing page is a grid of tiles, each heading linking to its page and
/// showing its state; waiting work is the line itself, styled as a badge;
/// the main action is the button beside the heading, as the registry names
/// it, and not a tile.
#[test]
fn landing_pages_show_state_and_their_main_action() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, id) = admin(app).await;
        seed_every_status(app, id).await;

        let (status, html) = get(app, &cookies, "/admin/content/overview").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(r#"<ul class="dashboard-grid landing-tiles" role="list">"#));
        assert!(
            html.contains(r#"<a href="/admin/content">Content</a></h2>"#),
            "only the heading text is the link: {html}"
        );
        assert_eq!(html.matches(" unpublished item").count(), 1);
        assert_eq!(html.matches(" waiting for moderation").count(), 1);
        let at = html.find(" waiting for moderation").unwrap();
        let open = html[..at].rfind('<').unwrap();
        assert!(
            html[open..].starts_with(r#"<p class="status-line status-line--waiting">"#),
            "the badge is the line itself: {}",
            &html[open..at]
        );
        let actions = &html[html.find("page-header__actions").unwrap()..];
        let actions = &actions[..actions.find("</div>").unwrap()];
        assert_eq!(
            button_text(actions, "/admin/content/add").as_deref(),
            Some(primary_label("/admin/content/add"))
        );
        assert!(
            !html.contains(r#"<a href="/admin/content/add">"#),
            "the main action is not a tile"
        );

        for (group, href, state) in [
            ("people", "/admin/people/add", " blocked account"),
            ("structure", "/admin/structure/types/add", ""),
        ] {
            let (status, html) = get(app, &cookies, &format!("/admin/{group}/overview")).await;
            assert_eq!(status, StatusCode::OK);
            let actions = &html[html.find("page-header__actions").unwrap()..];
            assert_eq!(
                button_text(actions, href).as_deref(),
                Some(primary_label(href)),
                "{group}"
            );
            if !state.is_empty() {
                assert_eq!(html.matches(state).count(), 1, "{group}");
            }
        }

        let (_, html) = get(app, &cookies, "/admin/reports/overview").await;
        assert_eq!(
            html.matches("error in the last 24 hours").count()
                + html.matches("errors in the last 24 hours").count(),
            1
        );
        assert!(
            html.contains(r#"<p class="status-line status-line--waiting">"#),
            "errors wait"
        );
        assert!(
            html.contains(r#"<p class="status-line">Last run "#)
                || html.contains("No run in the last 24 hours"),
            "the cron line: {html}"
        );
        assert!(
            !html.contains("button--primary"),
            "Reports has no main action"
        );
    });
}

/// A count never reveals what the viewer may not open: without `edit any
/// content` the content list filters row by row, so there is no Content
/// count; without the Comments page's permission, no comment count.
#[test]
fn a_status_line_is_shown_only_to_whoever_may_open_all_it_counts() {
    run_test(async {
        let app = shared_app().await;
        let (_, id) = admin(app).await;
        seed_every_status(app, id).await;

        let author = holding(
            app,
            &["access administration pages", "access content overview"],
        )
        .await;
        let (status, html) = get(app, &author, "/admin/content/overview").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(r#"<a href="/admin/content">"#), "{html}");
        assert!(!html.contains("unpublished item"), "{html}");
        assert!(!html.contains("waiting for moderation"));
        let (_, html) = get(app, &author, "/admin").await;
        assert!(!html.contains("unpublished item"));

        let editor = holding(app, &["access administration pages", "edit any content"]).await;
        let (_, html) = get(app, &editor, "/admin/content/overview").await;
        assert_eq!(html.matches(" unpublished item").count(), 1, "{html}");
        assert!(!html.contains("waiting for moderation"));
        let (_, html) = get(app, &editor, "/admin").await;
        assert_eq!(html.matches(" unpublished item").count(), 1);
        assert!(!html.contains("blocked account"));
    });
}

/// The Recent errors panel counts and lists its errors with one cutoff: an
/// error exactly at the cutoff is both counted and listed, and a second later
/// is neither, so the summary never counts an error the rows leave out.
#[test]
fn the_recent_errors_count_and_rows_share_one_cutoff() {
    run_test(async {
        let app = shared_app().await;
        let tag = tag();
        // Far in the future, so no other error falls in the windows below.
        let at = 4_000_000_000_i64;
        trovato_kernel::system_log::insert(
            &app.db,
            &[trovato_kernel::system_log::LogRecord {
                created: at,
                level: "error",
                log_type: "dashboard_test".to_string(),
                message: format!("Cutoff test failure {tag}"),
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
        let admin = UserContext::administrator(Uuid::nil(), vec![]);
        let day = 24 * 60 * 60;

        let panel =
            trovato_kernel::routes::admin_dashboard::errors_at(&app.state, &admin, at + day)
                .await
                .expect("the panel");
        let listed = panel
            .rows
            .iter()
            .filter(|r| r.text.args.values().any(|v| v.contains(&tag)))
            .count();
        assert_eq!(panel.summary.count, Some(1), "{:?}", panel.summary);
        assert_eq!(listed, 1, "the counted error is listed");

        let panel =
            trovato_kernel::routes::admin_dashboard::errors_at(&app.state, &admin, at + day + 1)
                .await
                .expect("the panel");
        assert_eq!(panel.summary.count, Some(0), "{:?}", panel.summary);
        assert!(
            !panel
                .rows
                .iter()
                .any(|r| r.text.args.values().any(|v| v.contains(&tag))),
            "an error past the cutoff is neither counted nor listed"
        );
    });
}
