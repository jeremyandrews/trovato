#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Comment author pages (B159, K-comment-author-pages): reply, edit own and
//! delete own, each a page of its own, each behind its own permission.
//!
//! Before, the only edit and delete controls were on the administrator's
//! moderation screen, so a member could neither correct nor withdraw their
//! own comment, and the item page's Reply link only worked with JavaScript.
//! And the comment API's PUT and DELETE asked for `edit any comment` and
//! `delete any comment`, which nothing declares: a moderator holding
//! `administer comments` who was not the site administrator got 403.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, defer_cleanup, extract_cookies, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateComment, CreateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A fresh content type, removed again when the test ends.
async fn content_type(app: &TestApp) -> String {
    app.ensure_plugin_enabled("trovato_comments").await;
    let name = format!("b159_pages_{}", &Uuid::now_v7().simple().to_string()[16..]);
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'core', '{}'::jsonb)",
    )
    .bind(&name)
    .execute(&app.db)
    .await
    .expect("seed type");
    app.cleanup_content_type_on_exit(&name);
    app.state
        .content_types()
        .replace(&name, &name, None, serde_json::json!({ "fields": [] }))
        .await
        .ok();
    name
}

/// A published item of `item_type`, deleted (with its comments) at the end.
async fn item(app: &TestApp, item_type: &str) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: format!("Pages {}", Uuid::now_v7().simple()),
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

/// A signed-in member holding `permissions`, and their id. The account and
/// its role go when the test ends, after the items (and so the comments).
async fn member(app: &TestApp, permissions: &[&str]) -> (String, Uuid) {
    let name = format!("b159_{}", Uuid::now_v7().simple());
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
    let mut granted = vec!["access content"];
    granted.extend_from_slice(permissions);
    common::grant_via_role(app, user_id, &granted).await;
    app.state.permissions().invalidate_all();
    (cookies, user_id)
}

/// A published comment by `author` on `item_id`.
async fn comment(app: &TestApp, item_id: Uuid, author: Uuid, body: &str) -> Uuid {
    app.state
        .comments()
        .create(
            CreateComment {
                item_id,
                parent_id: None,
                author_id: author,
                subject: None,
                body: body.to_string(),
                body_format: Some("plain_text".to_string()),
                status: Some(1),
            },
            &admin(),
        )
        .await
        .expect("create comment")
        .id
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
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
    (status, location, text_of(response).await)
}

/// The `_csrf` value of the first form on `html`.
fn csrf_field(html: &str) -> String {
    let marker = "name=\"_csrf\" value=\"";
    let start = html.find(marker).expect("a _csrf field") + marker.len();
    html[start..].split('"').next().unwrap().to_string()
}

/// The CSRF token of the page's head, for a JSON call.
async fn header_token(app: &TestApp, cookies: &str) -> (String, String) {
    let response = app
        .request_with_cookies(Request::get("/").body(Body::empty()).unwrap(), cookies)
        .await;
    let fresh = extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let html = text_of(response).await;
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    (
        cookies,
        html[start..].split('"').next().unwrap().to_string(),
    )
}

async fn post_form(app: &TestApp, cookies: &str, uri: &str, body: String) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::post(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, location)
}

async fn body_of(app: &TestApp, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT body FROM comment WHERE id = $1")
        .bind(id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
}

/// The jibnotes 403: a moderator with `administer comments`, who is not the
/// site administrator, edits and deletes through the comment API.
#[test]
fn a_moderator_edits_and_deletes_through_the_api() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app).await;
        let item_id = item(app, &item_type).await;
        let (_, writer) = member(app, &["post comments"]).await;
        let (moderator, _) = member(app, &["administer comments"]).await;
        let id = comment(app, item_id, writer, "Moderate me").await;

        let (moderator, token) = header_token(app, &moderator).await;
        let put = app
            .request_with_cookies(
                Request::put(format!("/api/v1/comment/{id}"))
                    .header("content-type", "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(r#"{"body":"Moderated"}"#))
                    .unwrap(),
                &moderator,
            )
            .await;
        assert_eq!(put.status(), StatusCode::OK, "{}", text_of(put).await);
        assert_eq!(body_of(app, id).await.as_deref(), Some("Moderated"));

        // CSRF tokens are single-use: the DELETE takes a fresh one.
        let (moderator, token) = header_token(app, &moderator).await;

        let delete = app
            .request_with_cookies(
                Request::delete(format!("/api/v1/comment/{id}"))
                    .header("X-CSRF-Token", &token)
                    .body(Body::empty())
                    .unwrap(),
                &moderator,
            )
            .await;
        assert_eq!(delete.status(), StatusCode::OK, "{}", text_of(delete).await);
        assert_eq!(body_of(app, id).await, None, "the comment is gone");
    });
}

/// The reply page: a form for the item, or for one comment with that comment
/// shown above it, posting to the comment route.
#[test]
fn the_reply_page_offers_a_form_for_the_item_and_for_a_comment() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app).await;
        let item_id = item(app, &item_type).await;
        let (cookies, me) = member(app, &["post comments", "skip comment approval"]).await;
        let parent = comment(app, item_id, me, "The parent remark").await;

        let (status, _, page) = get(app, &cookies, &format!("/comment/reply/{item_id}")).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(page.contains("Add new comment"));
        assert!(page.contains(&format!("action=\"/api/v1/item/{item_id}/comments\"")));

        let (status, _, page) =
            get(app, &cookies, &format!("/comment/reply/{item_id}/{parent}")).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(page.contains("The parent remark"), "the parent is shown");
        assert!(page.contains(&format!("name=\"parent_id\" value=\"{parent}\"")));

        // Posting the page's form makes a threaded reply.
        let token = csrf_field(&page);
        let (status, location) = post_form(
            app,
            &cookies,
            &format!("/api/v1/item/{item_id}/comments"),
            format!("_csrf={token}&parent_id={parent}&body=A+reply+from+the+page"),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert!(location.contains("comment=posted"), "{location}");
        let replies: i64 =
            sqlx::query_scalar("SELECT count(*) FROM comment WHERE parent_id = $1 AND body = $2")
                .bind(parent)
                .bind("A reply from the page")
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(replies, 1);

        // A reply link on the item page leads here.
        let (_, _, item_page) = get(app, &cookies, &format!("/item/{item_id}")).await;
        assert!(item_page.contains(&format!("href=\"/comment/reply/{item_id}/{parent}\"")));
    });
}

/// Who may not reply: a visitor is sent to log in and brought back; a member
/// without `post comments` is refused.
#[test]
fn the_reply_page_refuses_who_may_not_post() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app).await;
        let item_id = item(app, &item_type).await;

        let (status, location, _) = get(app, "", &format!("/comment/reply/{item_id}")).await;
        assert!(status.is_redirection(), "{status}");
        assert!(
            location.starts_with("/user/login?destination=%2Fcomment%2Freply%2F"),
            "{location}"
        );

        let (reader, _) = member(app, &[]).await;
        let (status, _, _) = get(app, &reader, &format!("/comment/reply/{item_id}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (poster, _) = member(app, &["post comments"]).await;
        let (status, _, _) = get(app, &poster, &format!("/comment/reply/{}", Uuid::now_v7())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no such item");
    });
}

/// Edit own: the author with `edit own comments` edits through a page with a
/// CSRF token; nobody else does, and nor does an author without it.
#[test]
fn an_author_edits_their_own_comment() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app).await;
        let item_id = item(app, &item_type).await;
        let (author, me) = member(app, &["post comments", "edit own comments"]).await;
        let (other, them) = member(app, &["post comments", "edit own comments"]).await;
        let (plain, plain_id) = member(app, &["post comments"]).await;
        let mine = comment(app, item_id, me, "Before the edit").await;
        let theirs = comment(app, item_id, them, "Someone else's").await;
        let plain_comment = comment(app, item_id, plain_id, "No edit permission").await;

        // The item page links Edit on my comment only.
        let (_, _, page) = get(app, &author, &format!("/item/{item_id}")).await;
        assert!(page.contains(&format!("href=\"/comment/{mine}/edit\"")));
        assert!(!page.contains(&format!("href=\"/comment/{theirs}/edit\"")));

        let (status, _, form) = get(app, &author, &format!("/comment/{mine}/edit")).await;
        assert_eq!(status, StatusCode::OK, "{form}");
        assert!(form.contains("Before the edit"));

        // Without the token nothing changes.
        let (status, _) = post_form(
            app,
            &author,
            &format!("/comment/{mine}/edit"),
            "_csrf=wrong&body=Forged".to_string(),
        )
        .await;
        assert!(!status.is_redirection(), "{status}");
        assert_eq!(body_of(app, mine).await.as_deref(), Some("Before the edit"));

        let (status, location) = post_form(
            app,
            &author,
            &format!("/comment/{mine}/edit"),
            format!("_csrf={}&body=After+the+edit", csrf_field(&form)),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert!(
            location.ends_with(&format!("?comment=updated#comment-{mine}")),
            "{location}"
        );
        assert_eq!(body_of(app, mine).await.as_deref(), Some("After the edit"));

        // Someone else's comment, and my comment without the permission.
        let (status, _, _) = get(app, &other, &format!("/comment/{mine}/edit")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = get(app, &plain, &format!("/comment/{plain_comment}/edit")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, location, _) = get(app, "", &format!("/comment/{mine}/edit")).await;
        assert!(status.is_redirection() && location.starts_with("/user/login"));
    });
}

/// Delete own: a confirmation page, then a POST with the CSRF token, for the
/// author with `delete own comments`. Without it, 403 — the original's
/// members could not delete even their own comments, and that stays the
/// default.
#[test]
fn an_author_deletes_their_own_comment_after_confirming() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app).await;
        let item_id = item(app, &item_type).await;
        let (author, me) = member(app, &["post comments", "delete own comments"]).await;
        let (plain, plain_id) = member(app, &["post comments", "edit own comments"]).await;
        let mine = comment(app, item_id, me, "Delete me").await;
        let kept = comment(app, item_id, plain_id, "Keep me").await;

        let (_, _, page) = get(app, &author, &format!("/item/{item_id}")).await;
        assert!(page.contains(&format!("href=\"/comment/{mine}/delete\"")));

        let (status, _, confirm) = get(app, &author, &format!("/comment/{mine}/delete")).await;
        assert_eq!(status, StatusCode::OK, "{confirm}");
        assert!(confirm.contains("Are you sure you want to delete this comment?"));
        assert_eq!(
            body_of(app, mine).await.as_deref(),
            Some("Delete me"),
            "a GET deletes nothing"
        );

        let (status, _) = post_form(
            app,
            &author,
            &format!("/comment/{mine}/delete"),
            "_csrf=wrong".to_string(),
        )
        .await;
        assert!(!status.is_redirection(), "{status}");
        assert!(body_of(app, mine).await.is_some(), "no token, no delete");

        let (status, location) = post_form(
            app,
            &author,
            &format!("/comment/{mine}/delete"),
            format!("_csrf={}", csrf_field(&confirm)),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert!(
            location.ends_with("?comment=deleted#comments"),
            "{location}"
        );
        assert_eq!(body_of(app, mine).await, None);

        // No `delete own comments`: refused at the page and at the POST.
        let (status, _, _) = get(app, &plain, &format!("/comment/{kept}/delete")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, _, page) = get(app, &plain, &format!("/item/{item_id}")).await;
        assert!(!page.contains(&format!("href=\"/comment/{kept}/delete\"")));
        let (status, _) = post_form(
            app,
            &plain,
            &format!("/comment/{kept}/delete"),
            format!("_csrf={}", csrf_field(&page)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body_of(app, kept).await.is_some());
    });
}
