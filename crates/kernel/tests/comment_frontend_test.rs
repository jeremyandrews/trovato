#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Comments on the front end: the thread under an item, and a form that works
//! with and without JavaScript.
//!
//! `templates/elements/comments.html` existed and was rendered by nothing — the
//! only comment template any route used was the admin one. The orphan could not
//! have worked either: its form posted `application/x-www-form-urlencoded` with
//! no CSRF field, at a JSON route that required a CSRF *header*.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, extract_cookies, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CommentStatus, CreateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE_SEED_LOCK: i64 = 0x_F405_0000_0001;
const ITEM_TYPE: &str = "comment_frontend_test";

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_item_type(app: &TestApp) {
    // The comment routes are behind the `trovato_comments` gate, and the thread
    // renderer asks `comments_if_enabled`, so on a database where the plugin was
    // never enabled there is no form, no thread and no route to post to.
    app.ensure_plugin_enabled("trovato_comments").await;

    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Comment Front End Test', 'Fixture type', true, 'Title', 'core', '{}'::jsonb) \
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
            "Comment Front End Test",
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
                title: format!("Front End Comments {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("comment front end test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// A logged-in commenter holding `post comments` and `skip comment approval`.
///
/// The bypass permission is deliberate: `comment_default_status` is one row in a
/// database every test binary shares, so a test that expected a posted comment to
/// be published would depend on whichever other binary was mid-test. Holding the
/// bypass makes the outcome this file's own business. The queue behaviour itself
/// is covered in `comment_moderation_test`, which owns that setting.
async fn commenter(app: &TestApp) -> String {
    let name = format!("frontend_{}", Uuid::now_v7().simple());
    let cookies = app
        .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find test user");

    let role_name = format!("frontend_role_{}", Uuid::now_v7().simple());
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (id, name) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = $2 RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(&role_name)
    .fetch_one(&app.db)
    .await
    .expect("create role");

    for permission in ["post comments", "access content", "skip comment approval"] {
        sqlx::query(
            "INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(role_id)
        .bind(permission)
        .execute(&app.db)
        .await
        .expect("grant permission");
    }
    sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(user_id)
        .bind(role_id)
        .execute(&app.db)
        .await
        .expect("assign role");
    app.state.permissions().invalidate_all();

    cookies
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Fetch the item page, following the session cookies given.
async fn item_page(app: &TestApp, cookies: &str, item_id: Uuid) -> String {
    let response = app
        .request_with_cookies(
            Request::get(format!("/item/{item_id}"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET /item/{item_id}");
    text_of(response).await
}

/// A CSRF token and the cookies that go with it.
async fn csrf(app: &TestApp, cookies: &str) -> (String, String) {
    let response = app
        .request_with_cookies(Request::get("/").body(Body::empty()).unwrap(), cookies)
        .await;
    let new_cookies = extract_cookies(&response);
    let cookies = if new_cookies.is_empty() {
        cookies.to_string()
    } else {
        new_cookies
    };
    let html = text_of(response).await;
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let token = html[start..].split('"').next().unwrap().to_string();
    (cookies, token)
}

fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The whole point: an item page renders its comment thread.
#[test]
fn an_item_page_renders_its_comment_thread() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;
        let cookies = commenter(app).await;

        // A published comment to find on the page.
        let (cookies, token) = csrf(app, &cookies).await;
        let posted = app
            .request_with_cookies(
                Request::post(format!("/api/item/{item_id}/comments"))
                    .header("content-type", "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(
                        serde_json::json!({"body": "A visible remark"}).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(posted.status(), StatusCode::OK, "JSON posting still works");

        let page = item_page(app, &cookies, item_id).await;

        assert!(
            page.contains("id=\"comments\""),
            "the comment section must be on the page"
        );
        assert!(
            page.contains("A visible remark"),
            "the comment body must be rendered, page was:\n{page}"
        );
        assert!(
            page.contains("data-comment-form"),
            "a permitted viewer gets the form"
        );
        assert!(
            page.contains("name=\"_csrf\""),
            "the form must carry a CSRF field, which is what the orphan template lacked"
        );
    });
}

/// Anonymous readers see the thread and a login prompt, not a form they cannot
/// submit.
#[test]
fn an_anonymous_reader_is_offered_a_login_rather_than_a_form() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;

        let page = item_page(app, "", item_id).await;

        assert!(page.contains("id=\"comments\""));
        assert!(
            !page.contains("data-comment-form"),
            "an anonymous reader cannot post, so must not be shown the form"
        );
        assert!(
            page.contains("to post a comment"),
            "the login prompt must be shown, page was:\n{page}"
        );
    });
}

/// A form-encoded post works: the token travels in the body, and the response is
/// a redirect back to the item rather than JSON.
#[test]
fn a_form_encoded_post_creates_the_comment_and_redirects() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;
        let cookies = commenter(app).await;
        let (cookies, token) = csrf(app, &cookies).await;

        let body = format!(
            "body={}&parent_id=&_csrf={}",
            urlencode("Posted without JavaScript"),
            urlencode(&token)
        );
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/item/{item_id}/comments"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "a browser posting a form must be redirected, not handed JSON"
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            location.starts_with(&format!("/item/{item_id}")),
            "redirect must return to the item, was {location}"
        );
        assert!(
            location.contains("comment=posted") && location.ends_with("#comments"),
            "the redirect must say what happened and land on the thread, was {location}"
        );

        let page = item_page(app, &cookies, item_id).await;
        assert!(
            page.contains("Posted without JavaScript"),
            "the comment must exist, page was:\n{page}"
        );
    });
}

/// A held comment says so on the page. Without this, a reader posting into a
/// moderated site sees their comment simply not appear.
///
/// Drives the notice through the query parameter the comment route redirects
/// with, rather than by switching the site into moderation: that setting is one
/// row shared by every test binary. `comment_moderation_test` owns it and covers
/// the redirect that produces this parameter.
#[test]
fn the_page_reports_a_comment_held_for_review() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;
        let cookies = commenter(app).await;

        let response = app
            .request_with_cookies(
                Request::get(format!("/item/{item_id}?comment=pending"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = text_of(response).await;

        assert!(
            page.contains("awaiting review"),
            "the page must tell the reader their comment is held, page was:\n{page}"
        );

        // And the other two outcomes the redirect can carry.
        for (flag, expected) in [("posted", "was posted"), ("error", "could not be posted")] {
            let response = app
                .request_with_cookies(
                    Request::get(format!("/item/{item_id}?comment={flag}"))
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await;
            let page = text_of(response).await;
            assert!(
                page.contains(expected),
                "the {flag} outcome must be reported, page was:\n{page}"
            );
        }
    });
}

/// A form post without a valid token creates nothing. The CSRF check moved to
/// the body for form submissions; it did not go away.
#[test]
fn a_form_post_with_a_bad_token_is_refused() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;
        let cookies = commenter(app).await;
        let (cookies, _) = csrf(app, &cookies).await;

        let body = format!(
            "body={}&parent_id=&_csrf=not-a-real-token",
            urlencode("Should never appear")
        );
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/item/{item_id}/comments"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;

        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            location.contains("comment=error"),
            "a refused submission must report an error, was {location}"
        );

        let page = item_page(app, &cookies, item_id).await;
        assert!(
            !page.contains("Should never appear"),
            "no comment may be created without a valid token"
        );
    });
}

/// The thread only shows what the public may see, so a pending comment is absent
/// from the rendered page even for the person who wrote it.
#[test]
fn the_rendered_thread_shows_only_published_comments() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app).await;
        let cookies = commenter(app).await;
        let (cookies, token) = csrf(app, &cookies).await;

        let posted = app
            .request_with_cookies(
                Request::post(format!("/api/item/{item_id}/comments"))
                    .header("content-type", "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(
                        serde_json::json!({"body": "Hidden by moderation"}).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        let created: serde_json::Value =
            serde_json::from_str(&text_of(posted).await).expect("json");
        let comment_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

        app.state
            .comments()
            .update(
                comment_id,
                trovato_kernel::models::UpdateComment {
                    body: None,
                    body_format: None,
                    status: Some(CommentStatus::Unpublished.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("unpublish");

        let page = item_page(app, &cookies, item_id).await;
        assert!(
            !page.contains("Hidden by moderation"),
            "an unpublished comment must not be rendered, page was:\n{page}"
        );
    });
}
