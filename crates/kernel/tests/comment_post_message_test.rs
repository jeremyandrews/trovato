#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The message a commenter sees after posting (B159, K-comment-post-message).
//!
//! The comment form's script posts JSON and, for a published comment, used to
//! reload the page. The page says "Your comment was posted." only for
//! `?comment=posted`, which a bare reload does not carry, so a reader with
//! JavaScript never saw it, and on a paged thread the new comment could be on
//! another page. The script now goes to the comment's own address,
//! `/comment/{id}?comment=posted`, which redirects to the page of the thread
//! that holds it, at the comment, with the message.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, defer_cleanup, extract_cookies, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

async fn content_type(app: &TestApp, settings: serde_json::Value) -> String {
    app.ensure_plugin_enabled("trovato_comments").await;
    let name = format!("b159_post_{}", &Uuid::now_v7().simple().to_string()[16..]);
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'core', $2)",
    )
    .bind(&name)
    .bind(&settings)
    .execute(&app.db)
    .await
    .expect("seed type");
    app.cleanup_content_type_on_exit(&name);
    let mut registered = settings;
    registered["fields"] = serde_json::json!([]);
    app.state
        .content_types()
        .replace(&name, &name, None, registered)
        .await
        .ok();
    name
}

async fn item(app: &TestApp, item_type: &str) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: format!("Post message {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM comment WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

async fn member(app: &TestApp) -> (String, Uuid) {
    let name = format!("b159p_{}", Uuid::now_v7().simple());
    let cookies = app
        .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find user");
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM comment WHERE author_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
    });
    common::grant_via_role(
        app,
        user_id,
        &["access content", "post comments", "skip comment approval"],
    )
    .await;
    app.state.permissions().invalidate_all();
    (cookies, user_id)
}

async fn comment_at(app: &TestApp, item_id: Uuid, author: Uuid, created: i64, status: i16) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO comment (id, item_id, author_id, body, body_format, status, created, changed) \
         VALUES ($1, $2, $3, 'Earlier', 'plain_text', $4, $5, $5)",
    )
    .bind(id)
    .bind(item_id)
    .bind(author)
    .bind(status)
    .bind(created)
    .execute(&app.db)
    .await
    .expect("insert comment");
    id
}

/// Post as comment-post.js does: JSON, the head's CSRF token in the header.
async fn post_json(app: &TestApp, cookies: &str, item_id: Uuid, body: &str) -> Uuid {
    let response = app
        .request_with_cookies(Request::get("/").body(Body::empty()).unwrap(), cookies)
        .await;
    let fresh = extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes);
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta") + marker.len();
    let token = html[start..].split('"').next().unwrap().to_string();
    let response = app
        .request_with_cookies(
            Request::post(format!("/api/v1/item/{item_id}/comments"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", token)
                .body(Body::from(serde_json::json!({ "body": body }).to_string()))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["id"].as_str().unwrap().parse().unwrap()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        location,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The script's destination says the comment was posted, on the page that
/// holds it.
#[test]
fn a_posted_comment_lands_on_its_page_with_the_message() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({ "comment_per_page": 2 })).await;
        let item_id = item(app, &item_type).await;
        let (cookies, me) = member(app).await;
        comment_at(app, item_id, me, 1_600_000_000, 1).await;
        comment_at(app, item_id, me, 1_600_000_001, 1).await;

        let id = post_json(app, &cookies, item_id, "Third, on page two").await;
        let (status, location, _) =
            get(app, &cookies, &format!("/comment/{id}?comment=posted")).await;
        assert!(status.is_redirection(), "{status}");
        assert_eq!(
            location,
            format!("/item/{item_id}?page=2&comment=posted#comment-{id}")
        );

        let (status, _, page) = get(app, &cookies, location.split('#').next().unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("Your comment was posted."), "the message");
        assert!(page.contains("Third, on page two"), "the comment");
    });
}

/// Only the outcomes the page knows are passed on; an id that is not one, a
/// missing comment and someone else's held comment are 404.
#[test]
fn the_comment_address_passes_on_only_what_it_knows() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let item_id = item(app, &item_type).await;
        let (cookies, me) = member(app).await;
        let (other, _) = member(app).await;
        let shown = comment_at(app, item_id, me, 1_600_000_000, 1).await;
        let held = comment_at(app, item_id, me, 1_600_000_001, 2).await;

        let (_, location, _) = get(app, "", &format!("/comment/{shown}?comment=%3Cb%3E")).await;
        assert_eq!(location, format!("/item/{item_id}#comment-{shown}"));

        for missing in ["3", &Uuid::now_v7().to_string()] {
            let (status, _, _) = get(app, "", &format!("/comment/{missing}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
        }
        let (status, _, _) = get(app, &other, &format!("/comment/{held}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "someone else's held comment");
        let (status, _, _) = get(app, &cookies, &format!("/comment/{held}")).await;
        assert!(status.is_redirection(), "the author's own held comment");
    });
}

/// The script itself: it goes to the comment's address with the outcome, and
/// no longer reloads, which is what lost the message.
#[test]
fn the_comment_script_goes_to_the_comment_with_the_outcome() {
    let script = std::fs::read_to_string(common::project_root().join("static/js/comment-post.js"))
        .expect("read comment-post.js");
    assert!(
        script.contains("'/comment/' + encodeURIComponent(comment.id) + '?comment=posted'"),
        "a published comment goes to its address with ?comment=posted"
    );
    assert!(
        !script.contains("location.reload()"),
        "a bare reload drops ?comment=posted, so the page never says the comment was posted"
    );
}
