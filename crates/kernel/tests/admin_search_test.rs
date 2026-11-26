#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Admin search: one search over pages, setting fields, content, people,
//! plugins and log messages, grouped under headings, and only what the
//! viewer may open. The command palette asks `/admin/search/suggest`, which
//! must give the same results as the page.
//!
//! Before, `/admin/search` matched page titles in the sidebar's registry
//! and nothing else: no settings, content, people, plugins or log messages,
//! no groups, no palette, and no way to choose or turn off a shortcut.
//!
//! Every row a test makes carries a fresh tag and is deleted when the test
//! ends. Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, defer_cleanup, run_test, shared_app, test_ip_for};
use trovato_kernel::models::item::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::routes::admin_search_index;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";

/// A fresh tag: lowercase hex, so it is never an ILIKE wildcard.
fn tag() -> String {
    format!("as{}", Uuid::now_v7().simple())
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b199search"))
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

async fn post_form(app: &TestApp, cookies: &str, uri: &str, form: String) -> StatusCode {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("x-forwarded-for", test_ip_for("b199search"))
            .body(Body::from(form))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

fn token(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let at = html.find(needle).expect("a form token") + needle.len();
    html[at..at + html[at..].find('"').unwrap()].to_string()
}

/// An administrator's session; the account goes when the test ends.
async fn admin(app: &TestApp) -> String {
    let name = common::username("asadmin");
    let cookies = app
        .create_and_login_admin(&name, PASSWORD, &format!("{name}@test.local"))
        .await;
    forget_user(app, &name).await;
    cookies
}

async fn forget_user(app: &TestApp, name: &str) {
    let id = common::user_id_of(app, name).await;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

/// A user holding `permissions`; the account goes when the test ends.
async fn holding(app: &TestApp, permissions: &[&str]) -> String {
    let (id, cookies) = common::user_holding(app, "asuser", permissions).await;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    cookies
}

/// A page item titled `title`, deleted when the test ends.
async fn item(app: &TestApp, title: &str) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("search test".to_string()),
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

/// A site log message containing `tag`, deleted when the test ends.
async fn log_message(app: &TestApp, tag: &str) {
    trovato_kernel::system_log::record_info(
        &app.db,
        "search_test",
        &format!("Search test message {tag}"),
        &[],
    )
    .await;
    let pool = app.db.clone();
    let like = format!("%{tag}%");
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM system_log WHERE message LIKE $1")
            .bind(like)
            .execute(&pool)
            .await;
    });
}

/// The result groups on a search page: each group's key and its links, in
/// page order.
fn page_groups(html: &str) -> Vec<(String, Vec<String>)> {
    html.split(r#"<section class="search-results" aria-labelledby="search-results-"#)
        .skip(1)
        .map(|section| {
            let key = section[..section.find('"').unwrap()].to_string();
            let body = &section[..section.find("</section>").unwrap()];
            let list = &body[..body.find("</ul>").unwrap()];
            let hrefs = list
                .match_indices(r#"class="landing-list__link" href=""#)
                .map(|(at, needle)| {
                    let from = at + needle.len();
                    list[from..from + list[from..].find('"').unwrap()].to_string()
                })
                .collect();
            (key, hrefs)
        })
        .collect()
}

fn hrefs_in<'a>(groups: &'a [(String, Vec<String>)], key: &str) -> &'a [String] {
    groups
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, hrefs)| hrefs.as_slice())
        .unwrap_or(&[])
}

/// The results are grouped under headings, and each kind of record is found:
/// a content title, an account, a log message, a setting field and a page.
#[test]
fn search_groups_pages_settings_content_people_and_log_messages() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app).await;
        let tag = tag();
        let id = item(app, &format!("Searchable {tag}")).await;
        let person = format!("asperson-{tag}");
        app.create_test_user(&person, PASSWORD, &format!("{person}@test.local"))
            .await;
        forget_user(app, &person).await;
        let person_id = common::user_id_of(app, &person).await;
        log_message(app, &tag).await;

        let (status, html) = get(app, &cookies, &format!("/admin/search?q={tag}")).await;
        assert_eq!(status, StatusCode::OK);
        let groups = page_groups(&html);
        let keys: Vec<&str> = groups.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["content", "people", "log"], "{html}");
        assert_eq!(
            hrefs_in(&groups, "content"),
            [format!("/admin/content/{id}/edit")]
        );
        assert_eq!(
            hrefs_in(&groups, "people"),
            [format!("/admin/people/{person_id}/edit")]
        );
        assert_eq!(hrefs_in(&groups, "log").len(), 1);
        assert!(hrefs_in(&groups, "log")[0].starts_with("/admin/reports/log?q="));
        for heading in ["Content", "People", "Log messages"] {
            assert!(html.contains(&format!(">{heading}</h2>")), "{heading}");
        }
        assert!(html.contains("3 results"), "the count is given");

        // A setting is found by its label and links to the field itself.
        let (_, html) = get(app, &cookies, "/admin/search?q=site+slogan").await;
        let groups = page_groups(&html);
        assert!(
            hrefs_in(&groups, "settings")
                .iter()
                .any(|h| h == "/admin/config/site#site_slogan"),
            "{groups:?}"
        );
        // A page by a synonym.
        let (_, html) = get(app, &cookies, "/admin/search?q=watchdog").await;
        let groups = page_groups(&html);
        assert!(
            hrefs_in(&groups, "pages")
                .iter()
                .any(|h| h == "/admin/reports/log"),
            "{groups:?}"
        );
        // A plugin on disk, by name, linking to its row on the plugins page.
        let (_, plugins) = get(app, &cookies, "/admin/plugins").await;
        let needle = r#"<th scope="row" id="plugin-"#;
        let at = plugins
            .find(needle)
            .expect("the plugins page lists a plugin")
            + needle.len();
        let name = &plugins[at..at + plugins[at..].find('"').unwrap()];
        let (_, html) = get(app, &cookies, &format!("/admin/search?q={name}")).await;
        let groups = page_groups(&html);
        assert!(
            hrefs_in(&groups, "plugins")
                .iter()
                .any(|h| *h == format!("/admin/plugins#plugin-{name}")),
            "{name}: {groups:?}"
        );
    });
}

/// A viewer sees only the groups whose listing they may open: the same
/// search finds nothing private for an editor, and an outsider gets neither
/// the page nor the suggestions.
#[test]
fn search_never_shows_what_the_viewer_may_not_open() {
    run_test(async {
        let app = shared_app().await;
        let tag = tag();
        let _ = item(app, &format!("Private {tag}")).await;
        let person = format!("asperson-{tag}");
        app.create_test_user(&person, PASSWORD, &format!("{person}@test.local"))
            .await;
        forget_user(app, &person).await;
        log_message(app, &tag).await;

        // May open the content listing, not people or the log.
        let editor = holding(app, &["access administration pages", "edit any content"]).await;
        let (status, html) = get(app, &editor, &format!("/admin/search?q={tag}")).await;
        assert_eq!(status, StatusCode::OK);
        let keys: Vec<String> = page_groups(&html).into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["content"], "{html}");
        let (_, json) = get(app, &editor, &format!("/admin/search/suggest?q={tag}")).await;
        assert!(!json.contains(&person), "{json}");
        assert!(!json.contains("search_test"), "{json}");

        // Settings on a form the editor may not open are not offered.
        let (_, html) = get(app, &editor, "/admin/search?q=site+slogan").await;
        assert!(hrefs_in(&page_groups(&html), "settings").is_empty());
        // Plugins on disk are not listed for someone who may not manage them.
        let (_, html) = get(app, &editor, "/admin/search?q=trovato").await;
        assert!(!html.contains("/admin/plugins#plugin-"), "{html}");

        // May open the admin but no listing at all.
        let visitor = holding(app, &["access administration pages"]).await;
        let (_, html) = get(app, &visitor, &format!("/admin/search?q={tag}")).await;
        assert!(page_groups(&html).is_empty(), "{html}");

        // May not use the admin.
        let outsider = holding(app, &[]).await;
        let (status, _) = get(app, &outsider, &format!("/admin/search?q={tag}")).await;
        assert_ne!(status, StatusCode::OK);
        let (status, body) = get(app, &outsider, &format!("/admin/search/suggest?q={tag}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!body.contains(&tag));
        let (status, _) = get(app, "", &format!("/admin/search/suggest?q={tag}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    });
}

/// Each setting the search links to is a field on its page: the anchor is
/// an element id there.
#[test]
fn every_setting_the_search_links_to_is_a_field_on_its_page() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app).await;
        let mut pages = std::collections::HashMap::new();
        let mut missing = Vec::new();
        for setting in admin_search_index::settings() {
            if !pages.contains_key(setting.page) {
                let (status, html) = get(app, &cookies, setting.page).await;
                assert_eq!(status, StatusCode::OK, "{}", setting.page);
                pages.insert(setting.page, html);
            }
            let html = &pages[setting.page];
            if !html.contains(&format!(r#"id="{}""#, setting.anchor)) {
                missing.push(format!("{}#{}", setting.page, setting.anchor));
            }
        }
        assert!(missing.is_empty(), "{missing:#?}");
    });
}

/// The palette's suggestions are the page's results: the same groups with
/// the same links in the same order, and the count to announce.
#[test]
fn the_palette_suggests_the_same_results_as_the_search_page() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app).await;
        let tag = tag();
        let _ = item(app, &format!("Palette {tag}")).await;
        log_message(app, &tag).await;
        for q in [tag.as_str(), "site", "content", "menu", "zzzznothing"] {
            let (_, html) = get(app, &cookies, &format!("/admin/search?q={q}")).await;
            let page = page_groups(&html);
            let (status, json) = get(app, &cookies, &format!("/admin/search/suggest?q={q}")).await;
            assert_eq!(status, StatusCode::OK);
            let json: serde_json::Value = serde_json::from_str(&json).expect("JSON");
            let suggested: Vec<(String, Vec<String>)> = json["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| {
                    (
                        g["key"].as_str().unwrap().to_string(),
                        g["hits"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|h| h["href"].as_str().unwrap().to_string())
                            .collect(),
                    )
                })
                .collect();
            assert_eq!(suggested, page, "{q}");
            let count: usize = page.iter().map(|(_, h)| h.len()).sum();
            assert_eq!(json["count"].as_u64().unwrap() as usize, count, "{q}");
            let summary = json["summary"].as_str().unwrap();
            assert!(summary.contains(&count.to_string()), "{q}: {summary}");
            for group in json["groups"].as_array().unwrap() {
                assert!(!group["title"].as_str().unwrap().is_empty());
                for hit in group["hits"].as_array().unwrap() {
                    assert!(!hit["label"].as_str().unwrap().is_empty());
                }
            }
        }
    });
}

/// The search shortcuts are the viewer's own: `/` is on until they turn it
/// off, and they may add Control+K or Alt+K. The banner's palette button
/// carries them for the script, and the preferences form shows them.
#[test]
fn the_search_shortcuts_are_saved_and_reach_the_palette_button() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app).await;
        let (_, html) = get(app, &cookies, "/admin").await;
        assert!(html.contains(r#"data-search-slash="on""#), "on by default");
        assert!(html.contains(r#"data-search-key="""#));
        assert!(html.contains(r#"aria-keyshortcuts="/""#));
        assert!(
            html.contains(r#"id="admin-palette""#),
            "the dialog is there"
        );

        let (_, form) = get(app, &cookies, "/admin/preferences").await;
        assert!(form.contains(r#"id="search_slash""#), "{form}");
        assert!(form.contains(r#"id="search_key""#));
        let status = post_form(
            app,
            &cookies,
            "/admin/preferences",
            format!(
                "_token={}&theme=system&language=&search_key=ctrl-k",
                urlencoding::encode(&token(&form))
            ),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (_, html) = get(app, &cookies, "/admin").await;
        assert!(html.contains(r#"data-search-slash="off""#), "turned off");
        assert!(html.contains(r#"data-search-key="ctrl-k""#));
        assert!(html.contains(r#"aria-keyshortcuts="Control+K Meta+K""#));

        let (_, form) = get(app, &cookies, "/admin/preferences").await;
        let status = post_form(
            app,
            &cookies,
            "/admin/preferences",
            format!(
                "_token={}&theme=system&language=&search_slash=1&search_key=nonsense",
                urlencoding::encode(&token(&form))
            ),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (_, html) = get(app, &cookies, "/admin").await;
        assert!(html.contains(r#"data-search-slash="on""#));
        assert!(
            html.contains(r#"data-search-key="""#),
            "an unknown key is none"
        );
    });
}
