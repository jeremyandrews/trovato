#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Comment moderation: the review queue, the spam status, and who skips them.
//!
//! Before this, comment status was two-valued and `create_comment` hardcoded
//! `status: Some(1)`, so every comment published the moment it was posted and
//! there was no way to hold one. The `skip comment approval` permission
//! `trovato_comments` declares was read by nothing.
//!
//! The notification rule (mail on becoming visible, not on being created) is
//! unit-tested in `routes::comment`, where the decision is a pure function.
//! What is pinned here is the storage and visibility behaviour, which needs a
//! database.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CommentStatus, CreateItem, SiteConfig};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_F404_0000_0001;

const ITEM_TYPE: &str = "comment_moderation_test";

/// `comment_default_status` is one row of site-wide configuration and every test
/// here reads or writes it, so one lock covers the file.
static DEFAULT_STATUS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_item_type(app: &TestApp) {
    // The comment routes are behind the `trovato_comments` gate, so on a database
    // where the plugin was never enabled every request here is a 404. This passed
    // locally, and in CI only because another file in the same shard had enabled
    // the plugin first — a shard reshuffle is all it took to break it.
    app.ensure_plugin_enabled("trovato_comments").await;

    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Comment Moderation Test', 'Fixture type', true, 'Title', 'core', '{}'::jsonb) \
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
            "Comment Moderation Test",
            Some("Fixture type"),
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();
}

/// Create a published item owned by `author`, so a comment on it has a content
/// author that is not the commenter.
async fn create_item(app: &TestApp, author: Uuid) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: format!("Moderation Fixture {}", Uuid::now_v7().simple()),
                author_id: author,
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("comment moderation test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// Create a logged-in commenter holding `permissions`, and return its cookies
/// and user id.
async fn commenter_with(app: &TestApp, permissions: &[&str]) -> (String, Uuid) {
    let name = format!("moderation_{}", Uuid::now_v7().simple());
    let cookies = app
        .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find test user");

    let role_name = format!("moderation_role_{}", Uuid::now_v7().simple());
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (id, name) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = $2 RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(&role_name)
    .fetch_one(&app.db)
    .await
    .expect("create role");

    for permission in permissions {
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

async fn set_default_status(app: &TestApp, value: Option<&str>) {
    match value {
        Some(v) => SiteConfig::set(
            &app.db,
            "comment_default_status",
            serde_json::Value::String(v.to_string()),
        )
        .await
        .expect("set default status"),
        None => {
            sqlx::query("DELETE FROM site_config WHERE key = 'comment_default_status'")
                .execute(&app.db)
                .await
                .map(|_| ())
                .expect("clear default status");
        }
    }
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// Fetch a CSRF token for the API, carrying the session cookies forward.
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

    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let token = html[start..]
        .split('"')
        .next()
        .expect("csrf token value")
        .to_string();

    (cookies, token)
}

/// Post a comment as `cookies`, returning the JSON response.
async fn post_comment(app: &TestApp, cookies: &str, item_id: Uuid) -> serde_json::Value {
    let (cookies, token) = csrf(app, cookies).await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &token)
                .body(Body::from(
                    serde_json::json!({"body": "A comment awaiting judgement"}).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK, "posting a comment");
    body_json(response).await
}

/// How many comments the public listing shows for an item.
async fn public_comment_count(app: &TestApp, item_id: Uuid) -> i64 {
    let response = app
        .request(
            Request::get(format!("/api/item/{item_id}/comments"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["total"].as_i64().unwrap_or(-1)
}

/// With the queue on, a comment is stored pending and is not published.
#[test]
fn a_new_comment_is_held_when_the_site_holds_comments() {
    run_test(async {
        let app = shared_app().await;
        let _guard = DEFAULT_STATUS.lock().await;
        ensure_item_type(app).await;
        set_default_status(app, Some("pending")).await;

        let (cookies, user_id) = commenter_with(app, &["post comments", "access content"]).await;
        let item_id = create_item(app, Uuid::nil()).await;
        assert_ne!(user_id, Uuid::nil(), "commenter is not the content author");

        let created = post_comment(app, &cookies, item_id).await;

        assert_eq!(
            created["status"].as_i64(),
            Some(i64::from(CommentStatus::Pending.as_i16())),
            "a held comment is stored pending, got {created}"
        );
        assert_eq!(
            public_comment_count(app, item_id).await,
            0,
            "a pending comment must not appear in the public listing"
        );

        set_default_status(app, None).await;
    });
}

/// `skip comment approval` is finally read: a commenter holding it bypasses the
/// queue even when the site holds comments.
#[test]
fn skip_comment_approval_bypasses_the_queue() {
    run_test(async {
        let app = shared_app().await;
        let _guard = DEFAULT_STATUS.lock().await;
        ensure_item_type(app).await;
        set_default_status(app, Some("pending")).await;

        let (cookies, _) = commenter_with(
            app,
            &["post comments", "access content", "skip comment approval"],
        )
        .await;
        let item_id = create_item(app, Uuid::nil()).await;

        let created = post_comment(app, &cookies, item_id).await;

        assert_eq!(
            created["status"].as_i64(),
            Some(i64::from(CommentStatus::Published.as_i16())),
            "the permission must skip the hold, got {created}"
        );
        assert_eq!(
            public_comment_count(app, item_id).await,
            1,
            "a bypassed comment is visible immediately"
        );

        set_default_status(app, None).await;
    });
}

/// Unset means publish immediately, which is what every comment did before the
/// setting existed. Upgrading a site must not silently start holding comments.
#[test]
fn an_unset_default_publishes_immediately() {
    run_test(async {
        let app = shared_app().await;
        let _guard = DEFAULT_STATUS.lock().await;
        ensure_item_type(app).await;
        set_default_status(app, None).await;

        let (cookies, _) = commenter_with(app, &["post comments", "access content"]).await;
        let item_id = create_item(app, Uuid::nil()).await;

        let created = post_comment(app, &cookies, item_id).await;

        assert_eq!(
            created["status"].as_i64(),
            Some(i64::from(CommentStatus::Published.as_i16())),
            "got {created}"
        );
        assert_eq!(public_comment_count(app, item_id).await, 1);
    });
}

/// A setting that is present but unrecognised holds comments rather than
/// publishing them: a comment wrongly held sits in a queue, while a comment
/// wrongly published is already on the site.
#[test]
fn an_unrecognised_default_fails_closed_into_the_queue() {
    run_test(async {
        let app = shared_app().await;
        let _guard = DEFAULT_STATUS.lock().await;
        ensure_item_type(app).await;
        set_default_status(app, Some("publsihed")).await;

        let (cookies, _) = commenter_with(app, &["post comments", "access content"]).await;
        let item_id = create_item(app, Uuid::nil()).await;

        let created = post_comment(app, &cookies, item_id).await;

        assert_eq!(
            created["status"].as_i64(),
            Some(i64::from(CommentStatus::Pending.as_i16())),
            "a typo must not publish, got {created}"
        );

        set_default_status(app, None).await;
    });
}

/// Approving a held comment publishes it and makes it visible.
#[test]
fn approving_a_held_comment_publishes_it() {
    run_test(async {
        let app = shared_app().await;
        let _guard = DEFAULT_STATUS.lock().await;
        ensure_item_type(app).await;
        set_default_status(app, Some("pending")).await;

        let (cookies, _) = commenter_with(app, &["post comments", "access content"]).await;
        let item_id = create_item(app, Uuid::nil()).await;
        let created = post_comment(app, &cookies, item_id).await;
        let comment_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

        // The moderation action, through the service the admin route uses.
        let updated = app
            .state
            .comments()
            .update(
                comment_id,
                trovato_kernel::models::UpdateComment {
                    body: None,
                    body_format: None,
                    status: Some(CommentStatus::Published.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("approve comment")
            .expect("comment exists");

        assert_eq!(updated.status, CommentStatus::Published.as_i16());
        assert_eq!(
            public_comment_count(app, item_id).await,
            1,
            "an approved comment becomes visible"
        );

        set_default_status(app, None).await;
    });
}

/// Spam is kept, not deleted, and is not visible — so a false positive can be
/// recovered and a classifier has something to learn from.
#[test]
fn a_comment_marked_spam_is_retained_and_invisible() {
    run_test(async {
        let app = shared_app().await;
        let _guard = DEFAULT_STATUS.lock().await;
        ensure_item_type(app).await;
        set_default_status(app, None).await;

        let (cookies, _) = commenter_with(app, &["post comments", "access content"]).await;
        let item_id = create_item(app, Uuid::nil()).await;
        let created = post_comment(app, &cookies, item_id).await;
        let comment_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(public_comment_count(app, item_id).await, 1);

        app.state
            .comments()
            .update(
                comment_id,
                trovato_kernel::models::UpdateComment {
                    body: None,
                    body_format: None,
                    status: Some(CommentStatus::Spam.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("mark spam");

        assert_eq!(
            public_comment_count(app, item_id).await,
            0,
            "spam must not be visible"
        );

        let still_there = app
            .state
            .comments()
            .load(comment_id)
            .await
            .expect("load")
            .expect("spam is retained, not deleted");
        assert_eq!(still_there.status, CommentStatus::Spam.as_i16());
    });
}

/// The moderation queue is reachable by status, which is what the admin filter
/// uses.
#[test]
fn pending_comments_are_listable_by_status() {
    run_test(async {
        let app = shared_app().await;
        let _guard = DEFAULT_STATUS.lock().await;
        ensure_item_type(app).await;
        set_default_status(app, Some("pending")).await;

        let (cookies, _) = commenter_with(app, &["post comments", "access content"]).await;
        let item_id = create_item(app, Uuid::nil()).await;
        let created = post_comment(app, &cookies, item_id).await;
        let comment_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

        let pending = app
            .state
            .comments()
            .list_by_status(CommentStatus::Pending.as_i16(), 100, 0)
            .await
            .expect("list pending");

        assert!(
            pending.iter().any(|c| c.id == comment_id),
            "the held comment must be in the pending queue"
        );

        set_default_status(app, None).await;
    });
}
