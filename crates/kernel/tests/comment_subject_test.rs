#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A comment's optional plain text subject (B163).
//!
//! The `comment` table had no column to hold one, so a site importing comments
//! with titles (Drupal comments have a subject field) had nowhere to put the
//! title but inside the body, which changed the body and could not be edited
//! separately. This covers: posting a comment with a subject stores it,
//! renders it escaped above the body, and makes it searchable; editing only
//! the subject leaves the body untouched; and a comment with no subject
//! renders with nothing extra.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, extract_cookies, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateComment, CreateItem, UpdateComment};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const ITEM_TYPE: &str = "comment_subject_test";
const TYPE_SEED_LOCK: i64 = 0x_B163_0000_0001;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_item_type(app: &TestApp) {
    app.ensure_plugin_enabled("trovato_comments").await;

    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Comment Subject Test', 'Fixture type', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .execute(&mut *tx)
    .await
    .expect("seed item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .replace(
            ITEM_TYPE,
            "Comment Subject Test",
            Some("Fixture type"),
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();
}

async fn create_item(app: &TestApp, title: &str) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("comment subject test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// A logged-in commenter holding `post comments` and `skip comment approval`,
/// so what this file asserts about a posted comment is not at the mercy of
/// `comment_default_status`, a setting every test binary shares.
async fn commenter(app: &TestApp) -> (String, Uuid) {
    let name = format!("subj_{}", Uuid::now_v7().simple());
    let cookies = app
        .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find test user");

    let role_name = format!("subj_role_{}", Uuid::now_v7().simple());
    let role_id = common::create_test_role(app, &role_name).await;

    for permission in [
        "post comments",
        "access content",
        "skip comment approval",
        "edit own comments",
    ] {
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

    (cookies, user_id)
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

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
    (
        cookies,
        html[start..].split('"').next().unwrap().to_string(),
    )
}

async fn post_json(
    app: &TestApp,
    cookies: &str,
    token: &str,
    uri: &str,
    json: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .request_with_cookies(
            Request::post(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", token)
                .body(Body::from(json.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let body = text_of(response).await;
    let value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// Posting a comment with a subject stores it, renders it escaped above the
/// body, and makes it findable by a word that appears only in the subject.
#[test]
fn a_comment_with_a_subject_is_stored_rendered_above_the_body_and_searchable() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let tag = Uuid::now_v7().simple().to_string();
        let subject_word = format!("zqsubj{tag}");
        let body_word = format!("zqbody{tag}");
        let item_id = create_item(app, &format!("Subject fixture {tag}")).await;
        let (cookies, _) = commenter(app).await;
        let (cookies, token) = csrf(app, &cookies).await;

        let subject = format!("<script>alert(1)</script> {subject_word}");
        let (status, created) = post_json(
            app,
            &cookies,
            &token,
            &format!("/api/item/{item_id}/comments"),
            serde_json::json!({
                "subject": subject,
                "body": format!("A remark about {body_word}"),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(
            created["subject"].as_str(),
            Some(subject.as_str()),
            "the stored subject comes back verbatim in the API response: {created}"
        );

        let page = item_page(app, &cookies, item_id).await;
        assert!(
            page.contains(&format!("A remark about {body_word}")),
            "the body must still be rendered, page was:\n{page}"
        );
        assert!(
            page.contains("&lt;script&gt;"),
            "the subject is plain text: it must be escaped, never rendered as markup, page was:\n{page}"
        );
        assert!(
            !page.contains("<script>alert(1)</script>"),
            "the raw script tag must never appear unescaped, page was:\n{page}"
        );

        let subject_at = page
            .find(&subject_word)
            .unwrap_or_else(|| panic!("the subject must be on the page: {page}"));
        let body_at = page
            .find(&format!("A remark about {body_word}"))
            .unwrap_or_else(|| panic!("the body must be on the page: {page}"));
        assert!(
            subject_at < body_at,
            "the subject must render above the body, subject at {subject_at}, body at {body_at}"
        );

        // Searchable: a word that appears only in the subject finds the item.
        let hits: Vec<Uuid> = app
            .state
            .search()
            .search_unrecorded(
                &subject_word,
                &trovato_kernel::search::SearchVisibility::public([LIVE_STAGE_ID]),
                50,
                0,
            )
            .await
            .expect("search")
            .results
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(
            hits,
            vec![item_id],
            "a word only in a comment's subject must find the item it is on"
        );
    });
}

/// A comment posted with no subject at all renders with nothing extra: no
/// subject heading, and the API response carries no subject.
#[test]
fn a_comment_posted_with_no_subject_renders_with_nothing_extra() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let tag = Uuid::now_v7().simple().to_string();
        let item_id = create_item(app, &format!("No subject fixture {tag}")).await;
        let (cookies, _) = commenter(app).await;
        let (cookies, token) = csrf(app, &cookies).await;

        let (status, created) = post_json(
            app,
            &cookies,
            &token,
            &format!("/api/item/{item_id}/comments"),
            serde_json::json!({ "body": format!("Plain remark {tag}") }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert!(
            created.get("subject").is_none() || created["subject"].is_null(),
            "no subject was sent, so none must come back: {created}"
        );

        let page = item_page(app, &cookies, item_id).await;
        assert!(page.contains(&format!("Plain remark {tag}")));
        assert!(
            !page.contains("comment__subject"),
            "a comment with no subject must not render a subject heading at all, page was:\n{page}"
        );
    });
}

/// An empty subject (whitespace only) is the same as no subject: trimmed and
/// stored as NULL, not as an empty string.
#[test]
fn a_whitespace_only_subject_is_stored_as_no_subject() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let tag = Uuid::now_v7().simple().to_string();
        let item_id = create_item(app, &format!("Whitespace subject fixture {tag}")).await;
        let (cookies, _) = commenter(app).await;
        let (cookies, token) = csrf(app, &cookies).await;

        let (status, created) = post_json(
            app,
            &cookies,
            &token,
            &format!("/api/item/{item_id}/comments"),
            serde_json::json!({ "subject": "   ", "body": format!("Remark {tag}") }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert!(
            created.get("subject").is_none() || created["subject"].is_null(),
            "a whitespace-only subject must be stored as no subject: {created}"
        );

        let comment_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        let stored: Option<String> =
            sqlx::query_scalar("SELECT subject FROM comment WHERE id = $1")
                .bind(comment_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(stored, None, "the column itself must hold NULL");
    });
}

/// A subject over the stored length limit is refused with a 400, not a
/// database error.
#[test]
fn a_subject_over_the_length_limit_is_refused() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app, "Length limit fixture").await;
        let (cookies, _) = commenter(app).await;
        let (cookies, token) = csrf(app, &cookies).await;

        let too_long = "x".repeat(256);
        let (status, _) = post_json(
            app,
            &cookies,
            &token,
            &format!("/api/item/{item_id}/comments"),
            serde_json::json!({ "subject": too_long, "body": "Body text" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    });
}

/// Editing only the subject through the JSON API leaves the body untouched —
/// the JSON API's `subject` key is absent-means-unchanged, the same shape
/// `body` already has.
#[test]
fn editing_only_the_subject_leaves_the_body_unchanged() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        let item_id = create_item(app, "Edit subject fixture").await;
        let (cookies, user_id) = commenter(app).await;

        let comment = app
            .state
            .comments()
            .create(
                CreateComment {
                    item_id,
                    parent_id: None,
                    author_id: user_id,
                    subject: Some("Original subject".to_string()),
                    body: "Original body".to_string(),
                    body_format: Some("plain_text".to_string()),
                    status: Some(1),
                },
                &admin(),
            )
            .await
            .expect("create comment");

        let (cookies, token) = csrf(app, &cookies).await;

        let response = app
            .request_with_cookies(
                Request::put(format!("/api/v1/comment/{}", comment.id))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(
                        serde_json::json!({ "subject": "New subject only" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            text_of(response).await
        );

        let row: (Option<String>, String) =
            sqlx::query_as("SELECT subject, body FROM comment WHERE id = $1")
                .bind(comment.id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("New subject only"));
        assert_eq!(row.1, "Original body", "the body must be untouched");

        // And the reverse: updating only the body leaves an existing subject
        // untouched, through the model layer directly.
        app.state
            .comments()
            .update(
                comment.id,
                UpdateComment {
                    body: Some("Changed body only".to_string()),
                    body_format: None,
                    status: None,
                    subject: None,
                },
                &admin(),
            )
            .await
            .expect("update body only");
        let row: (Option<String>, String) =
            sqlx::query_as("SELECT subject, body FROM comment WHERE id = $1")
                .bind(comment.id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("New subject only"));
        assert_eq!(row.1, "Changed body only");
    });
}
