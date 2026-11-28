#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The finer admin permissions an author holds on a Drupal site (B156).
//!
//! `access content overview` opens the content list, showing what the viewer
//! may see; the "Add content" chooser opens to whoever may create some type
//! and lists those types; `access site reports` opens the site log; `delete
//! own content` deletes one's own items from the list, one at a time or in
//! bulk; and `create/edit/delete category terms` open the term forms without
//! `administer categories`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const BROWSER: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

struct Answer {
    status: StatusCode,
    body: String,
}

async fn answer(response: axum::response::Response) -> Answer {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Answer {
        status,
        // Autoescape writes a printed `/` as `&#x2F;`; read the page as a
        // browser would.
        body: String::from_utf8_lossy(&bytes).replace("&#x2F;", "/"),
    }
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> Answer {
    let request = Request::get(path)
        .header(header::ACCEPT, BROWSER)
        .body(Body::empty())
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

async fn post_form(app: &TestApp, path: &str, cookies: &str, body: String) -> Answer {
    let request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", test_ip_for("b156-delegated"))
        .body(Body::from(body))
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

/// A CSRF token for this session, read from a page that carries one.
async fn token(app: &TestApp, page: &str, cookies: &str) -> String {
    let a = get(app, page, cookies).await;
    let marker = r#"name="_token" value=""#;
    let start = a
        .body
        .find(marker)
        .unwrap_or_else(|| panic!("no token on {page}: {} {}", a.status, a.body))
        + marker.len();
    let end = a.body[start..].find('"').unwrap();
    a.body[start..start + end].to_string()
}

async fn member(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String) {
    let (id, cookies) = user_holding(app, prefix, permissions).await;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
    (id, cookies)
}

async fn item(app: &TestApp, author: Uuid, status: i16, title: &str) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: title.to_string(),
                author_id: author,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b156".to_string()),
            },
            &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .unwrap()
        .id;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&db)
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

const AUTHOR: &[&str] = &[
    "access content",
    "access content overview",
    // Drupal's content overview lists one's own unpublished items to whoever
    // holds this; so does Trovato's, through the item's own access check.
    "view own unpublished content",
    "create page content",
    "edit own content",
    "delete own content",
];

#[test]
fn an_author_opens_the_content_list_and_sees_what_they_may_see() {
    run_test(async {
        let app = shared_app().await;
        let tag = Uuid::now_v7().simple().to_string();
        let (author, cookies) = member(app, "b156-author", AUTHOR).await;
        let (other, _) = member(app, "b156-other", &[]).await;
        let mine = format!("b156 my draft {tag}");
        let theirs = format!("b156 their draft {tag}");
        let public = format!("b156 their post {tag}");
        let my_draft = item(app, author, 0, &mine).await;
        item(app, other, 0, &theirs).await;
        let their_post = item(app, other, 1, &public).await;

        let a = get(app, "/admin/content?type=page", &cookies).await;
        assert_eq!(a.status, StatusCode::OK, "{}", a.body);
        assert!(a.body.contains(&mine), "their own draft is listed");
        assert!(a.body.contains(&public), "a published item is listed");
        assert!(!a.body.contains(&theirs), "another's draft is not");
        // Edit and delete are offered on what they may change, not elsewhere.
        assert!(a.body.contains(&format!("/content/{my_draft}/delete")));
        assert!(!a.body.contains(&format!("/content/{their_post}/delete")));
        assert!(a.body.contains(&format!("/item/{my_draft}/edit")));
        assert!(!a.body.contains(&format!("/{their_post}/edit")));
        // The bulk actions offered are the ones they hold: delete, not publish.
        assert!(a.body.contains("Delete selected"), "{}", a.body);
        assert!(!a.body.contains("Publish selected"), "{}", a.body);

        // A viewer who may change nothing is offered no bulk action.
        let (_, looker) = member(app, "b156-looker", &["access content overview"]).await;
        let l = get(app, "/admin/content", &looker).await;
        assert_eq!(l.status, StatusCode::OK);
        assert!(!l.body.contains("bulk-action"), "{}", l.body);

        // Without the permission it stays closed.
        let (_, plain) = member(app, "b156-noview", &["access content"]).await;
        assert_eq!(
            get(app, "/admin/content", &plain).await.status,
            StatusCode::FORBIDDEN
        );
    });
}

#[test]
fn the_add_content_chooser_lists_the_types_one_may_create() {
    run_test(async {
        let app = shared_app().await;
        // A second type, so the chooser has one to leave out whatever ran
        // before this test.
        sqlx::query(
            "INSERT INTO item_type (type, label, description, plugin) \
             VALUES ('b156_other', 'Other', '', 'core') ON CONFLICT (type) DO NOTHING",
        )
        .execute(&app.db)
        .await
        .expect("seed a second content type");
        let (db, state) = (app.db.clone(), app.state.clone());
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item_type WHERE type = 'b156_other'")
                .execute(&db)
                .await;
            let _ = state.content_types().reload_from_db().await;
        });
        app.state
            .content_types()
            .reload_from_db()
            .await
            .expect("reload content types");
        let (_, cookies) = member(app, "b156-chooser", &["create page content"]).await;
        let a = get(app, "/admin/content/add", &cookies).await;
        assert_eq!(a.status, StatusCode::OK, "{}", a.body);
        assert!(a.body.contains("/admin/content/add/page"), "{}", a.body);
        let others = app
            .state
            .content_types()
            .list_all()
            .await
            .into_iter()
            .filter(|t| t.machine_name != "page")
            .count();
        let offered = a.body.matches("/admin/content/add/").count();
        assert!(others > 0, "the fixture has other types");
        assert_eq!(offered, 1, "only the type they may create: {}", a.body);

        let (_, plain) = member(app, "b156-nocreate", &["access content"]).await;
        assert_eq!(
            get(app, "/admin/content/add", &plain).await.status,
            StatusCode::FORBIDDEN
        );
    });
}

#[test]
fn access_site_reports_opens_the_site_log() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = member(app, "b156-reports", &["access site reports"]).await;
        let a = get(app, "/admin/reports/log", &cookies).await;
        assert_eq!(a.status, StatusCode::OK, "{}", a.body);
        let (_, plain) = member(app, "b156-noreports", &["access content"]).await;
        assert_eq!(
            get(app, "/admin/reports/log", &plain).await.status,
            StatusCode::FORBIDDEN
        );
    });
}

#[test]
fn an_author_deletes_their_own_content_and_nobody_elses() {
    run_test(async {
        let app = shared_app().await;
        let (author, cookies) = member(app, "b156-deleter", AUTHOR).await;
        let (other, _) = member(app, "b156-owner", &[]).await;
        let mine = item(app, author, 1, "b156 mine").await;
        let theirs = item(app, other, 1, "b156 theirs").await;

        let t = token(app, "/admin/content", &cookies).await;
        post_form(
            app,
            &format!("/admin/content/{theirs}/delete"),
            &cookies,
            format!("_token={t}"),
        )
        .await;
        assert!(
            exists(app, theirs).await,
            "another's item is not theirs to delete"
        );
        assert_eq!(
            get(app, &format!("/admin/content/{theirs}/delete"), &cookies)
                .await
                .status,
            StatusCode::FORBIDDEN,
            "nor is its delete page"
        );
        let page = get(app, &format!("/admin/content/{mine}/delete"), &cookies).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        let a = post_form(
            app,
            &format!("/admin/content/{mine}/delete"),
            &cookies,
            common::form_fields(&page.body, "content_delete_form"),
        )
        .await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        assert!(!exists(app, mine).await, "their own item is deleted");

        // In bulk: their own goes, another's stays.
        let mine = item(app, author, 1, "b156 mine again").await;
        let list = get(app, "/admin/content", &cookies).await;
        let fields = common::form_fields(&list.body, "content_bulk_form");
        let a = post_form(
            app,
            "/admin/content/bulk",
            &cookies,
            format!("{fields}&action=delete&ids%5B%5D={mine}&ids%5B%5D={theirs}"),
        )
        .await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        assert!(!exists(app, mine).await);
        assert!(exists(app, theirs).await);
    });
}

#[test]
fn term_permissions_open_the_term_forms_without_administer_categories() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_categories").await;
        let categories = app.state.categories();
        let id = format!("b156_{}", &Uuid::now_v7().simple().to_string()[20..]);
        categories
            .create_category(
                serde_json::from_value(serde_json::json!({"id": id, "label": "B156 tags"}))
                    .unwrap(),
            )
            .await
            .unwrap();
        let db = app.db.clone();
        let cleanup_id = id.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM category_tag WHERE category_id = $1")
                .bind(&cleanup_id)
                .execute(&db)
                .await;
            let _ = sqlx::query("DELETE FROM category WHERE id = $1")
                .bind(&cleanup_id)
                .execute(&db)
                .await;
        });

        let (_, creator) = member(app, "b156-tagger", &["create category terms"]).await;
        let add = format!("/admin/structure/categories/{id}/tags/add");
        let t = token(app, &add, &creator).await;
        let a = post_form(
            app,
            &add,
            &creator,
            format!("_token={t}&_form_build_id=x&label=B156+term"),
        )
        .await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        let tags = categories.list_tags(&id).await.unwrap();
        assert_eq!(tags.len(), 1, "the term is created");
        let tag = tags[0].id;

        // Creating is not editing or deleting.
        let edit = format!("/admin/structure/tags/{tag}/edit");
        assert_eq!(
            get(app, &edit, &creator).await.status,
            StatusCode::FORBIDDEN
        );
        let (_, editor) = member(app, "b156-tag-editor", &["edit category terms"]).await;
        let t = token(app, &edit, &editor).await;
        let a = post_form(
            app,
            &edit,
            &editor,
            format!("_token={t}&_form_build_id=x&label=B156+renamed"),
        )
        .await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        assert_eq!(
            categories.get_tag(tag).await.unwrap().unwrap().label,
            "B156 renamed"
        );
        let delete = format!("/admin/structure/tags/{tag}/delete");
        post_form(app, &delete, &editor, format!("_token={t}")).await;
        assert!(
            categories.get_tag(tag).await.unwrap().is_some(),
            "not deleted"
        );

        let (_, deleter) = member(app, "b156-tag-deleter", &["delete category terms"]).await;
        let t = token(
            app,
            &format!("/admin/structure/categories/{id}/tags"),
            &deleter,
        )
        .await;
        let a = post_form(app, &delete, &deleter, format!("_token={t}")).await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        assert!(categories.get_tag(tag).await.unwrap().is_none(), "deleted");

        // None of them administers the category itself.
        assert_eq!(
            get(app, "/admin/structure/categories/add", &creator)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        // The workflow stages are tags, but not theirs to change.
        assert_eq!(
            get(app, "/admin/structure/categories/stages/tags/add", &creator)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let live = trovato_sdk::types::LIVE_STAGE_UUID;
        assert_eq!(
            get(app, &format!("/admin/structure/tags/{live}/edit"), &editor)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let list = get(app, "/admin/structure/categories", &creator).await;
        assert!(!list.body.contains("categories/stages/"), "{}", list.body);
        let (_, plain) = member(app, "b156-untagged", &["access content"]).await;
        assert_eq!(get(app, &add, &plain).await.status, StatusCode::FORBIDDEN);
    });
}
