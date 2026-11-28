#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The admin page anatomy (docs/admin/design.md section 3), held for every
//! page the shell draws, and the pages the shell itself owns: the group
//! landing pages, your preferences, admin search, and the sidebar's
//! remembered state.
//!
//! Before B199 the admin had no skip link, no `<main id="main">`, an `h2`
//! where the page heading belonged, a `<title>` that never named the page, no
//! landing pages, no preferences and no search: every test here fails there.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b199anat"))
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
        String::from_utf8_lossy(&bytes).replace("&#x2F;", "/"),
    )
}

async fn post(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    content_type: &str,
    body: String,
    extra: &[(&str, &str)],
) -> StatusCode {
    let mut request = Request::post(uri)
        .header("x-forwarded-for", test_ip_for("b199anat"))
        .header("content-type", content_type);
    for (k, v) in extra {
        request = request.header(*k, *v);
    }
    app.request_with_cookies(request.body(Body::from(body)).unwrap(), cookies)
        .await
        .status()
}

async fn admin(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// Delete the users this test made (every name starting `{prefix}-`): a test
/// run leaves nothing behind. Their roles go with the role tracker, their
/// preferences and role assignments with the account.
async fn forget(app: &TestApp, prefix: &str) {
    sqlx::query("DELETE FROM users WHERE name LIKE $1")
        .bind(format!("{prefix}-%"))
        .execute(&app.db)
        .await
        .expect("delete this test's users");
}

fn between<'a>(html: &'a str, open: &str, close: &str) -> &'a str {
    let start = html
        .find(open)
        .unwrap_or_else(|| panic!("{open} is on the page"));
    let rest = &html[start + open.len()..];
    &rest[..rest.find(close).unwrap_or(rest.len())]
}

/// Tags stripped, whitespace collapsed.
fn text_of(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in fragment.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Everything docs/admin/design.md section 3 promises, on one page.
fn assert_anatomy(path: &str, html: &str) {
    let body = between(html, "<body", "</body>");
    let first_link = &body[body.find("<a ").expect("a link")..];
    assert!(
        first_link.starts_with("<a class=\"skip-link\" href=\"#main\">"),
        "{path}: the first link on the page skips to the main content"
    );
    assert_eq!(body.matches("<main ").count(), 1, "{path}: one main");
    assert!(
        body.contains("<main class=\"admin-main\" id=\"main\" tabindex=\"-1\">"),
        "{path}: main is #main"
    );
    assert!(
        body.contains("<header class=\"admin-masthead\">"),
        "{path}: a banner"
    );
    assert!(
        body.contains(
            "<nav class=\"admin-sidebar\" id=\"admin-nav\" aria-label=\"Administration\">"
        ),
        "{path}: the sidebar is a navigation landmark named Administration"
    );
    assert!(
        body.contains("role=\"search\""),
        "{path}: a search landmark"
    );
    assert_eq!(body.matches("<h1").count(), 1, "{path}: exactly one h1");
    assert!(
        !body.contains("role=\"tablist\""),
        "{path}: local tasks are links, not tabs"
    );

    let h1 = text_of(between(body, "<h1", "</h1>").split_once('>').unwrap().1);
    assert!(!h1.is_empty(), "{path}: the h1 has text");
    let title = between(html, "<title>", "</title>");
    assert!(
        title.starts_with(&h1),
        "{path}: the <title> leads with the h1: {title:?} vs {h1:?}"
    );
    assert!(
        !title.contains("<!--"),
        "{path}: the title marker was filled"
    );
    if path != "/admin" {
        let crumbs = between(
            body,
            "<nav class=\"breadcrumb\" aria-label=\"Breadcrumb\">",
            "</nav>",
        );
        let current = between(crumbs, "aria-current=\"page\">", "</li>");
        assert_eq!(
            text_of(current),
            h1,
            "{path}: the breadcrumb ends with the page, marked current"
        );
        assert!(
            crumbs.contains("href=\"/admin\""),
            "{path}: the breadcrumb starts at the Dashboard"
        );
    }
    let lang = between(html, "<html lang=\"", "\"");
    assert!(!lang.is_empty(), "{path}: <html lang>");
    assert!(
        html.contains(" dir=\"ltr\"") || html.contains(" dir=\"rtl\""),
        "{path}: <html dir>"
    );
}

#[test]
fn every_kind_of_admin_page_has_the_shell_anatomy() {
    run_test(async {
        let app = shared_app().await;
        common::ensure_plugin_tables(app, "trovato_comments");
        app.ensure_plugin_enabled("trovato_comments").await;
        let cookies = admin(app, "b199anat").await;
        for path in [
            "/admin",
            "/admin/content/overview",
            "/admin/content",
            "/admin/content/add",
            "/admin/structure/types",
            "/admin/structure/stages",
            "/admin/structure/stages/add",
            "/admin/people",
            "/admin/people/roles",
            "/admin/appearance",
            "/admin/plugins",
            "/admin/config/site",
            "/admin/reports/log",
            "/admin/preferences",
            "/admin/search?q=user",
            "/admin/recovery",
            "/admin/content/comments",
        ] {
            let (status, html) = get(app, &cookies, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_anatomy(path, &html);
        }
        forget(app, "b199anat").await;
    });
}

/// The page heading leads the title, then the group, then the site; the
/// breadcrumb runs Dashboard › group › page.
#[test]
fn the_title_and_breadcrumb_name_the_page_then_its_group() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199title").await;
        let (_, html) = get(app, &cookies, "/admin/structure/stages").await;
        let title = between(&html, "<title>", "</title>");
        assert!(title.starts_with("Stages, Configuration, "), "{title}");
        let crumbs = between(&html, "aria-label=\"Breadcrumb\">", "</nav>");
        assert!(crumbs.contains("href=\"/admin\""));
        assert!(crumbs.contains("href=\"/admin/config/overview\""));

        // A landing page is its group: named once.
        let (_, html) = get(app, &cookies, "/admin/people/overview").await;
        let title = between(&html, "<title>", "</title>");
        assert!(title.starts_with("People, "), "{title}");
        assert!(!title.starts_with("People, People"), "{title}");
        forget(app, "b199title").await;
    });
}

/// Each group's landing page lists only what the viewer may open, and a group
/// with nothing for them does not exist for them.
#[test]
fn a_landing_page_lists_only_pages_the_viewer_may_open() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199land").await;
        let (status, html) = get(app, &cookies, "/admin/people/overview").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("href=\"/admin/people\""));
        assert!(html.contains("href=\"/admin/people/roles\""));

        let (_, editor) = user_holding(
            app,
            "b199land-ed",
            &["access administration pages", "edit any content"],
        )
        .await;
        let (status, html) = get(app, &editor, "/admin/content/overview").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("href=\"/admin/content\""));
        let (status, _) = get(app, &editor, "/admin/config/overview").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "nothing in Configuration is open to a content editor"
        );
        let (status, _) = get(app, &editor, "/admin/plugins").await;
        assert_ne!(status, StatusCode::OK, "the page keeps its own gate");
        forget(app, "b199land").await;
    });
}

/// Search finds registry pages by title, description and synonym, and only
/// the ones the viewer may open.
#[test]
fn admin_search_finds_pages_and_never_shows_one_the_viewer_may_not_open() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199srch").await;
        let (status, html) = get(app, &cookies, "/admin/search?q=roles").await;
        assert_eq!(status, StatusCode::OK);
        let results = between(&html, "<main", "</main>");
        assert!(
            results.contains("href=\"/admin/people/roles\""),
            "{results}"
        );

        let (_, editor) = user_holding(
            app,
            "b199srch-ed",
            &["access administration pages", "edit any content"],
        )
        .await;
        let (status, html) = get(app, &editor, "/admin/search?q=plugins").await;
        assert_eq!(status, StatusCode::OK);
        let results = between(&html, "<main", "</main>");
        assert!(!results.contains("href=\"/admin/plugins\""), "{results}");

        // The query is printed escaped.
        let (_, html) = get(app, &cookies, "/admin/search?q=%3Cscript%3Ex").await;
        assert!(!html.contains("<script>x"));
        forget(app, "b199srch").await;
    });
}

/// The theme and language are the viewer's own, and change the shell:
/// `data-theme`, and `lang`/`dir` (right to left for the pseudo locale).
#[test]
fn preferences_set_the_theme_and_the_admin_language_and_direction() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199pref").await;
        let (status, html) = get(app, &cookies, "/admin/preferences").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("data-theme=\"system\""));
        let fields = common::form_fields(&html, "admin_preferences_form");

        let status = post(
            app,
            &cookies,
            "/admin/preferences",
            "application/x-www-form-urlencoded",
            format!("{fields}&theme=dark&language="),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (_, html) = get(app, &cookies, "/admin/preferences").await;
        assert!(html.contains("data-theme=\"dark\""));
        assert!(
            html.contains("role=\"status\""),
            "the saved message is a status message"
        );

        // A language that is not offered is refused, with an alert.
        let fields = common::form_fields(&html, "admin_preferences_form");
        let status = post(
            app,
            &cookies,
            "/admin/preferences",
            "application/x-www-form-urlencoded",
            format!("{fields}&theme=dark&language=xx-nope"),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Without the right token, nothing is saved.
        let status = post(
            app,
            &cookies,
            "/admin/preferences",
            "application/x-www-form-urlencoded",
            "_token=wrong&theme=light&language=".to_string(),
            &[],
        )
        .await;
        assert_ne!(status, StatusCode::SEE_OTHER);
        let (_, html) = get(app, &cookies, "/admin").await;
        assert!(html.contains("data-theme=\"dark\""));
        forget(app, "b199pref").await;
    });
}

/// Whether the sidebar group `slug` is drawn open: its button says expanded
/// and its list is not marked collapsed.
fn group_open(html: &str, slug: &str) -> bool {
    let expanded = html.contains(&format!(
        "aria-expanded=\"true\" aria-controls=\"admin-nav-{slug}\""
    ));
    let collapsed = html.contains(&format!("id=\"admin-nav-{slug}\" data-collapsed"));
    assert_ne!(
        expanded, collapsed,
        "{slug}: the button and the list disagree"
    );
    expanded
}

/// Only the current page's group is open by default, and the Dashboard
/// opens none. A group the viewer opens by hand is remembered, per user, on
/// every page, until they close it; an unknown group and a request from
/// another site are refused.
#[test]
fn the_sidebar_opens_where_you_are_and_remembers_groups_opened_by_hand() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b245nav").await;
        let slugs = [
            "content",
            "structure",
            "appearance",
            "people",
            "extend",
            "config",
            "reports",
        ];

        let (_, html) = get(app, &cookies, "/admin").await;
        for slug in slugs {
            assert!(
                !group_open(&html, slug),
                "the Dashboard opens no group: {slug}"
            );
        }
        let (_, html) = get(app, &cookies, "/admin/content").await;
        for slug in slugs {
            assert_eq!(group_open(&html, slug), slug == "content", "{slug}");
        }
        assert!(html.contains("data-group=\"content\" data-current>"));
        assert!(html.contains("data-group=\"reports\">"));

        let toggle = |group: &str, open: bool, site: &'static str| {
            let body = format!(r#"{{"group":"{group}","open":{open}}}"#);
            let cookies = cookies.clone();
            async move {
                post(
                    app,
                    &cookies,
                    "/admin/preferences/nav",
                    "application/json",
                    body,
                    &[("sec-fetch-site", site)],
                )
                .await
            }
        };
        assert_eq!(
            toggle("reports", true, "same-origin").await,
            StatusCode::NO_CONTENT
        );
        let (_, html) = get(app, &cookies, "/admin/content").await;
        assert!(group_open(&html, "reports"), "opened by hand");
        assert!(group_open(&html, "content"), "the current group");
        assert!(!group_open(&html, "people"));
        let (_, html) = get(app, &cookies, "/admin").await;
        assert!(group_open(&html, "reports"), "remembered on every page");
        assert!(!group_open(&html, "content"));

        assert_eq!(
            toggle("nonsense", true, "same-origin").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            toggle("reports", false, "cross-site").await,
            StatusCode::FORBIDDEN
        );
        let (_, html) = get(app, &cookies, "/admin").await;
        assert!(
            group_open(&html, "reports"),
            "the refused request changed nothing"
        );

        assert_eq!(
            toggle("reports", false, "same-origin").await,
            StatusCode::NO_CONTENT
        );
        let (_, html) = get(app, &cookies, "/admin/reports/log").await;
        assert!(group_open(&html, "reports"), "the current group stays open");
        let (_, html) = get(app, &cookies, "/admin/content").await;
        assert!(!group_open(&html, "reports"), "closed by hand");

        let status = post(
            app,
            "",
            "/admin/preferences/nav",
            "application/json",
            r#"{"group":"reports","open":true}"#.to_string(),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        forget(app, "b245nav").await;
    });
}

/// The stored preferences object of `user`, as saved.
async fn stored_prefs(app: &TestApp, user: uuid::Uuid) -> Option<serde_json::Value> {
    sqlx::query_scalar("SELECT prefs FROM admin_preference WHERE user_id = $1")
        .bind(user)
        .fetch_optional(&app.db)
        .await
        .unwrap()
}

/// `toggle_group` writes one group in one statement whatever the row holds:
/// no row, a row with no `expanded` key (the theme survives, and the pre-B245
/// `collapsed` list goes), a slug not yet in the list and one already in it.
#[test]
fn toggling_a_group_saves_only_that_group_for_any_stored_row() {
    run_test(async {
        use trovato_kernel::routes::admin_nav::Preferences;
        let app = shared_app().await;
        let make = |_label: &'static str| async move {
            let name = common::username("b245tog");
            app.create_test_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
            common::user_id_of(app, &name).await
        };

        // No row at all.
        let fresh = make("a").await;
        assert_eq!(stored_prefs(app, fresh).await, None);
        Preferences::toggle_group(&app.db, fresh, "people", true)
            .await
            .unwrap();
        assert_eq!(
            stored_prefs(app, fresh).await,
            Some(serde_json::json!({ "expanded": ["people"] }))
        );

        // A row with no `expanded` key, opening: the theme survives and the
        // old `collapsed` list is dropped.
        let opener = make("b").await;
        Preferences::merge(
            &app.db,
            opener,
            serde_json::json!({ "theme": "dark", "collapsed": ["reports"] }),
        )
        .await
        .unwrap();
        Preferences::toggle_group(&app.db, opener, "reports", true)
            .await
            .unwrap();
        assert_eq!(
            stored_prefs(app, opener).await,
            Some(serde_json::json!({ "theme": "dark", "expanded": ["reports"] }))
        );

        // A row with no `expanded` key, closing: an empty list, never null.
        let closer = make("c").await;
        Preferences::merge(&app.db, closer, serde_json::json!({ "theme": "light" }))
            .await
            .unwrap();
        Preferences::toggle_group(&app.db, closer, "config", false)
            .await
            .unwrap();
        assert_eq!(
            stored_prefs(app, closer).await,
            Some(serde_json::json!({ "theme": "light", "expanded": [] }))
        );
        let prefs = Preferences::load(&app.db, closer).await;
        assert_eq!(prefs.theme.as_deref(), Some("light"));
        assert!(prefs.expanded.is_empty());

        // A slug not in the list is appended; one already in it is not
        // added twice; closing removes only it.
        Preferences::toggle_group(&app.db, fresh, "reports", true)
            .await
            .unwrap();
        Preferences::toggle_group(&app.db, fresh, "people", true)
            .await
            .unwrap();
        assert_eq!(
            Preferences::load(&app.db, fresh).await.expanded,
            vec!["people".to_string(), "reports".to_string()]
        );
        Preferences::toggle_group(&app.db, fresh, "extend", false)
            .await
            .unwrap();
        Preferences::toggle_group(&app.db, fresh, "people", false)
            .await
            .unwrap();
        assert_eq!(
            Preferences::load(&app.db, fresh).await.expanded,
            vec!["reports".to_string()]
        );
        forget(app, "b245tog").await;
    });
}

/// The preferences form (theme, language) and the sidebar's opened groups
/// are saved by different requests. Saved at the same moment, neither puts
/// back the other's old value: each writes only its own fields.
#[test]
fn saving_the_sidebar_state_and_the_theme_together_keeps_both() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b199race");
        app.create_test_admin(
            &name,
            "correct-horse-battery-staple",
            &format!("{name}@test.local"),
        )
        .await;
        let user = common::user_id_of(app, &name).await;
        use trovato_kernel::routes::admin_nav::Preferences;

        for round in 0..25 {
            let theme = if round % 2 == 0 { "dark" } else { "light" };
            let open = round % 2 == 0;
            let (a, b) = tokio::join!(
                Preferences::merge(
                    &app.db,
                    user,
                    serde_json::json!({ "theme": theme, "language": null })
                ),
                Preferences::toggle_group(&app.db, user, "people", open),
            );
            a.unwrap();
            b.unwrap();
            let prefs = Preferences::load(&app.db, user).await;
            assert_eq!(
                prefs.theme.as_deref(),
                Some(theme),
                "round {round}: the theme was lost"
            );
            let expected: Vec<String> = if open {
                vec!["people".to_string()]
            } else {
                Vec::new()
            };
            assert_eq!(
                prefs.expanded, expected,
                "round {round}: the groups were lost"
            );
        }
        forget(app, "b199race").await;
    });
}

/// The sidebar marks the current page, and the current group's disclosure
/// starts open.
#[test]
fn the_sidebar_marks_the_current_page() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199cur").await;
        let (_, html) = get(app, &cookies, "/admin/people/roles").await;
        let nav = between(&html, "aria-label=\"Administration\">", "</nav>");
        assert!(nav.contains("href=\"/admin/people/roles\" aria-current=\"page\""));
        assert_eq!(nav.matches("aria-current=\"page\"").count(), 1);
        assert!(nav.contains("aria-expanded=\"true\" aria-controls=\"admin-nav-people\""));
        forget(app, "b199cur").await;
    });
}
