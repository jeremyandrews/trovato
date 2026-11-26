#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The admin listing pattern (B199 Part 3, `docs/admin/design.md` section 9).
//!
//! - A destructive action asks first on a page of its own, so the question is
//!   asked with JavaScript off too. Before, a Delete button asked only
//!   through the script's `confirm()`; with JavaScript off it deleted at once.
//! - A bulk delete names how many items it will delete.
//! - Every admin table has a caption and scoped headers.
//! - The content listing filters by text, sorts by a column button that
//!   carries `aria-sort`, says how many matched, and pages with links that
//!   keep the filter.

mod common;

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn root() -> PathBuf {
    common::project_root()
}

fn templates(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            templates(&path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

fn admin_templates() -> Vec<(String, String)> {
    let mut files = Vec::new();
    templates(&root().join("templates/admin"), &mut files);
    files
        .into_iter()
        .map(|p| {
            let name = p
                .strip_prefix(root())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

/// No admin template asks only through the script: every form that deletes
/// goes through the confirmation page, and nothing uses `data-confirm`.
#[test]
fn every_delete_asks_first_without_javascript() {
    let mut problems = Vec::new();
    for (name, text) in admin_templates() {
        if text.contains("data-confirm") {
            problems.push(format!("{name}: asks only through data-confirm"));
        }
        for (at, _) in text.match_indices("<form") {
            let tag = &text[at..at + text[at..].find('>').unwrap_or(0)];
            let Some(start) = tag.find("action=\"") else {
                continue;
            };
            let action = &tag[start + 8..];
            let action = &action[..action.find('"').unwrap_or(action.len())];
            let deletes = action.split('?').next().unwrap_or("").ends_with("/delete");
            // A confirmation page's own form is the confirmed step.
            let is_confirm_step = name.ends_with("confirm.html");
            if deletes && !is_confirm_step && !action.contains("?confirm") {
                problems.push(format!("{name}: {action} deletes without asking"));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Every admin table has a caption, and each column header a scope.
#[test]
fn every_admin_table_has_a_caption_and_scoped_headers() {
    let mut problems = Vec::new();
    for (name, text) in admin_templates() {
        if name.contains("/macros/") {
            continue;
        }
        for (at, _) in text.match_indices("<table") {
            let rest = &text[at..];
            let end = rest.find("</table>").unwrap_or(rest.len());
            let table = &rest[..end];
            if !table.contains("<caption") {
                problems.push(format!("{name}: a table without a caption"));
            }
            for (th, _) in table.match_indices("<th") {
                let tag = &table[th..th + table[th..].find('>').unwrap_or(0)];
                // `<thead>` is not a header cell.
                if tag.starts_with("<thead") {
                    continue;
                }
                if !tag.contains("scope=") {
                    problems.push(format!("{name}: {tag}> has no scope"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

async fn body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b199lists"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body(response).await)
}

/// POST a form; returns the status, the `Location` and the body.
async fn post(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    form: String,
) -> (StatusCode, String, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::REFERER, "http://localhost/admin/content?q=kept")
                .header("x-forwarded-for", test_ip_for("b199lists"))
                .body(Body::from(form))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (status, location, body(response).await)
}

fn token(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let at = html.find(needle).expect("a form token") + needle.len();
    html[at..at + html[at..].find('"').unwrap()].to_string()
}

async fn admin(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    let prefix = prefix.to_string();
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name LIKE $1")
            .bind(format!("{prefix}-%"))
            .execute(&pool)
            .await;
    });
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
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
                log: Some("listing test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

async fn exists(app: &TestApp, id: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
        == 1
}

fn encode(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// **F3.** With JavaScript off, Delete leads to a page that names what will be
/// deleted; nothing is deleted until its button is pressed.
#[test]
fn a_delete_asks_on_its_own_page_before_it_deletes() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199listsdel").await;
        let title = format!("Doomed {}", Uuid::new_v4().simple());
        let id = item(app, &title).await;

        let (_, page) = get(app, &cookies, &format!("/admin/content?q={}", encode(&title))).await;
        assert!(
            page.contains(&format!(r#"action="/admin/content/{id}/delete?confirm""#)),
            "the row's delete form asks first"
        );
        let form = format!(
            "_token={}&_confirm={}&_confirm_label={}",
            encode(&token(&page)),
            encode(&format!("Delete {title}? This cannot be undone.")),
            encode(&format!("Delete {title}")),
        );
        let (status, location, _) =
            post(app, &cookies, &format!("/admin/content/{id}/delete?confirm"), form).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, "/admin/confirm");
        assert!(exists(app, id).await, "nothing is deleted before confirming");

        let (status, confirm) = get(app, &cookies, "/admin/confirm").await;
        assert_eq!(status, StatusCode::OK);
        assert!(confirm.contains(&format!("<h1")), "the page has its heading");
        assert!(confirm.contains(&format!("Delete {title}? This cannot be undone.")));
        assert!(
            confirm.contains(&format!(r#"action="&#x2F;admin&#x2F;content&#x2F;{id}&#x2F;delete""#)),
            "the button posts to the delete itself, without the marker"
        );
        assert!(
            confirm.contains(&format!(r#"class="button button--danger">Delete {title}</button>"#)),
            "the button names what it deletes"
        );
        assert!(
            confirm.contains(r#"href="&#x2F;admin&#x2F;content?q=kept""#),
            "Cancel goes back to the listing as it was"
        );

        let (status, location, _) = post(
            app,
            &cookies,
            &format!("/admin/content/{id}/delete"),
            format!("_token={}", encode(&token(&confirm))),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert_eq!(location, "/admin/content");
        assert!(!exists(app, id).await, "confirming deletes");
    });
}

/// A form token that does not verify never reaches the page: the handler
/// refuses it, as it did before.
#[test]
fn a_forged_delete_is_refused_not_asked_about() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199listsforge").await;
        let id = item(app, &format!("Kept {}", Uuid::new_v4().simple())).await;
        let (status, location, _) = post(
            app,
            &cookies,
            &format!("/admin/content/{id}/delete?confirm"),
            "_token=forged&_confirm=Anything%20at%20all".to_string(),
        )
        .await;
        assert_ne!(location, "/admin/confirm", "{status}");
        assert!(exists(app, id).await);
    });
}

/// A bulk delete names how many items it will delete; another bulk action
/// does not ask.
#[test]
fn a_bulk_delete_names_how_many_items() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199listsbulk").await;
        let tag = Uuid::new_v4().simple().to_string();
        let a = item(app, &format!("Bulk A {tag}")).await;
        let b = item(app, &format!("Bulk B {tag}")).await;

        let (_, page) = get(app, &cookies, &format!("/admin/content?q={tag}")).await;
        assert!(page.contains(r#"action="/admin/content/bulk?confirm=delete""#));
        assert!(page.contains("data-select-all"), "a select all box");
        assert!(page.contains("data-selected-count"), "a live selected count");
        let token = token(&page);
        let ids = format!("&ids%5B%5D={a}&ids%5B%5D={b}");

        let (status, location, _) = post(
            app,
            &cookies,
            "/admin/content/bulk?confirm=delete",
            format!("_token={}&action=delete{ids}", encode(&token)),
        )
        .await;
        assert_eq!((status, location.as_str()), (StatusCode::SEE_OTHER, "/admin/confirm"));
        let (_, confirm) = get(app, &cookies, "/admin/confirm").await;
        assert!(
            confirm.contains("Delete 2 selected items? This cannot be undone."),
            "the question names how many"
        );
        assert!(confirm.contains(">Delete 2 items</button>"));
        assert_eq!(confirm.matches(r#"name="ids[]""#).count(), 2);
        assert!(exists(app, a).await && exists(app, b).await);
        let token = self::token(&confirm);

        // Unpublishing is not destructive: it is done at once.
        let (status, location, _) = post(
            app,
            &cookies,
            "/admin/content/bulk?confirm=delete",
            format!("_token={}&action=unpublish{ids}", encode(&token)),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert_ne!(location, "/admin/confirm");
    });
}

/// The content listing: a text filter, sort buttons with `aria-sort`, a
/// result count, and the filtered empty state.
#[test]
fn the_content_listing_filters_sorts_and_counts() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "b199listsfilter").await;
        let tag = Uuid::new_v4().simple().to_string();
        item(app, &format!("Alpha {tag}")).await;
        item(app, &format!("Beta {tag}")).await;
        item(app, &format!("Unrelated {}", Uuid::new_v4().simple())).await;

        let (status, page) = get(app, &cookies, &format!("/admin/content?q={tag}&sort=title")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(r#"role="search""#), "the filter is a search landmark");
        assert!(page.contains(r#"name="q""#) && page.contains(&format!(r#"value="{tag}""#)));
        assert!(page.contains("2 results"), "the count says how many matched");
        assert!(page.contains(r#"id="list-live""#), "a live region for the count");
        let alpha = page.find(&format!("Alpha {tag}")).expect("Alpha is listed");
        let beta = page.find(&format!("Beta {tag}")).expect("Beta is listed");
        assert!(alpha < beta, "sorted by title, ascending");
        assert!(!page.contains("Unrelated "), "the filter leaves others out");
        assert!(page.contains(r#"aria-sort="ascending""#));
        assert!(
            page.contains(r#"form="list-sort" name="sort" value="-title""#),
            "pressing the sorted column again sorts it the other way"
        );
        assert!(
            page.contains(&format!(r#"name="q" value="{tag}""#)),
            "sorting keeps the filter"
        );

        let (_, page) = get(app, &cookies, &format!("/admin/content?q={tag}&sort=-title")).await;
        assert!(page.find(&format!("Beta {tag}")).unwrap() < page.find(&format!("Alpha {tag}")).unwrap());
        assert!(page.contains(r#"aria-sort="descending""#));

        // An unknown column falls back to the default order.
        let (status, _) = get(app, &cookies, &format!("/admin/content?q={tag}&sort=id;drop")).await;
        assert_eq!(status, StatusCode::OK);

        let (_, page) = get(app, &cookies, "/admin/content?q=no-such-title-at-all-b199").await;
        assert!(page.contains("No content matches this filter."));
        assert!(page.contains(r#"href="/admin/content""#), "a way to clear it");
    });
}
