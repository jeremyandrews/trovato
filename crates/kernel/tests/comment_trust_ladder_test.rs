#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The comment trust ladder: an account with approved comments skips the queue.
//!
//! The precedence rules are unit-tested in `models::comment` as a pure function.
//! What needs a database is the count that feeds them — which comments make an
//! account trusted — and the fact that the status the route stores and reports is
//! the one the ladder decided.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{
    Comment, CommentStatus, CreateComment, CreateItem, SiteConfig, UpdateComment,
};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE_SEED_LOCK: i64 = 0x_F407_0000_0001;
const ITEM_TYPE: &str = "trust_ladder_test";

/// Both settings this file writes are site-wide.
static SETTINGS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
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
         VALUES ($1, 'Trust Ladder Test', 'fixture', true, 'Title', 'core', '{}'::jsonb) \
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
            "Trust Ladder Test",
            Some("fixture"),
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
                title: format!("Trust Ladder {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("trust ladder test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// A commenter holding `post comments` only — no approval bypass.
async fn commenter(app: &TestApp) -> (String, Uuid) {
    let name = format!("ladder_{}", Uuid::now_v7().simple());
    let cookies = app
        .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find user");

    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (id, name) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = $2 RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(format!("ladder_role_{}", Uuid::now_v7().simple()))
    .fetch_one(&app.db)
    .await
    .expect("create role");

    for permission in ["post comments", "access content"] {
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

/// Give `author` `count` published comments, and one of each status that must not
/// count toward trust.
async fn seed_history(app: &TestApp, author: Uuid, item_id: Uuid, published: usize) {
    for _ in 0..published {
        let comment = app
            .state
            .comments()
            .create(
                CreateComment {
                    item_id,
                    parent_id: None,
                    author_id: author,
                    body: "An earlier, approved remark".to_string(),
                    body_format: Some("filtered_html".to_string()),
                    status: Some(CommentStatus::Published.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("seed published comment");
        assert_eq!(comment.status, CommentStatus::Published.as_i16());
    }

    // Noise that must not earn trust.
    for status in [
        CommentStatus::Pending,
        CommentStatus::Unpublished,
        CommentStatus::Spam,
    ] {
        app.state
            .comments()
            .create(
                CreateComment {
                    item_id,
                    parent_id: None,
                    author_id: author,
                    body: "Not evidence of anything".to_string(),
                    body_format: Some("filtered_html".to_string()),
                    status: Some(status.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("seed non-approved comment");
    }
}

async fn set_settings(app: &TestApp, default_status: Option<&str>, threshold: Option<i64>) {
    match default_status {
        Some(v) => SiteConfig::set(&app.db, "comment_default_status", serde_json::json!(v))
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
    match threshold {
        Some(t) => SiteConfig::set(&app.db, "comment_trust_threshold", serde_json::json!(t))
            .await
            .expect("set threshold"),
        None => {
            sqlx::query("DELETE FROM site_config WHERE key = 'comment_trust_threshold'")
                .execute(&app.db)
                .await
                .map(|_| ())
                .expect("clear threshold");
        }
    }
}

async fn clear_settings(app: &TestApp) {
    set_settings(app, None, None).await;
}

/// Post a comment and return the status the route reports.
async fn post_comment_status(app: &TestApp, cookies: &str, item_id: Uuid) -> i64 {
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
    let token = html[start..].split('"').next().unwrap().to_string();

    let response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &token)
                .body(Body::from(
                    serde_json::json!({"body": "A new remark"}).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK, "posting a comment");
    let bytes = to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("read body");
    let created: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

    created["status"]
        .as_i64()
        .expect("a status in the response")
}

/// A new account waits in the queue. This is the case the ladder exists to
/// distinguish, so it is asserted first.
#[test]
fn a_new_account_waits_for_review() {
    run_test(async {
        let app = shared_app().await;
        let _guard = SETTINGS.lock().await;
        ensure_item_type(app).await;
        set_settings(app, Some("pending"), Some(3)).await;

        let (cookies, _) = commenter(app).await;
        let item_id = create_item(app).await;

        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Pending.as_i16()),
            "an account with no history must wait"
        );

        clear_settings(app).await;
    });
}

/// An account with enough approved comments skips the wait.
#[test]
fn an_established_account_skips_the_queue() {
    run_test(async {
        let app = shared_app().await;
        let _guard = SETTINGS.lock().await;
        ensure_item_type(app).await;
        set_settings(app, Some("pending"), Some(3)).await;

        let (cookies, user_id) = commenter(app).await;
        let item_id = create_item(app).await;
        seed_history(app, user_id, item_id, 3).await;

        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Published.as_i16()),
            "three approved comments must earn the bypass"
        );

        clear_settings(app).await;
    });
}

/// Only *published* comments count. Otherwise a spammer earns trust by posting
/// into the queue, which would make the ladder worse than useless.
#[test]
fn comments_awaiting_review_or_marked_spam_earn_no_trust() {
    run_test(async {
        let app = shared_app().await;
        let _guard = SETTINGS.lock().await;
        ensure_item_type(app).await;
        set_settings(app, Some("pending"), Some(3)).await;

        let (cookies, user_id) = commenter(app).await;
        let item_id = create_item(app).await;
        // Nine comments, none of them approved.
        for _ in 0..3 {
            seed_history(app, user_id, item_id, 0).await;
        }

        assert_eq!(
            Comment::approved_count_for_author(&app.db, user_id)
                .await
                .expect("count"),
            0,
            "none of those statuses is evidence of anything"
        );
        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Pending.as_i16()),
            "posting into the queue must not earn a bypass"
        );

        clear_settings(app).await;
    });
}

/// Approval is what promotes an account: the same author crosses the threshold
/// when a moderator approves their third comment.
#[test]
fn approval_is_what_advances_an_account() {
    run_test(async {
        let app = shared_app().await;
        let _guard = SETTINGS.lock().await;
        ensure_item_type(app).await;
        set_settings(app, Some("pending"), Some(3)).await;

        let (cookies, user_id) = commenter(app).await;
        let item_id = create_item(app).await;
        seed_history(app, user_id, item_id, 2).await;

        // Two approved: still waiting.
        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Pending.as_i16())
        );

        // A moderator approves the comment that was just held.
        let held = app
            .state
            .comments()
            .list_by_status(CommentStatus::Pending.as_i16(), 200, 0)
            .await
            .expect("list pending")
            .into_iter()
            .find(|c| c.author_id == user_id)
            .expect("the held comment");
        app.state
            .comments()
            .update(
                held.id,
                UpdateComment {
                    body: None,
                    body_format: None,
                    status: Some(CommentStatus::Published.as_i16()),
                },
                &admin(),
            )
            .await
            .expect("approve");

        // Third approval reached: the next comment goes straight up.
        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Published.as_i16()),
            "the third approval must lift the account over the threshold"
        );

        clear_settings(app).await;
    });
}

/// A threshold of zero turns the ladder off, and history stops mattering.
#[test]
fn a_zero_threshold_disables_the_ladder() {
    run_test(async {
        let app = shared_app().await;
        let _guard = SETTINGS.lock().await;
        ensure_item_type(app).await;
        set_settings(app, Some("pending"), Some(0)).await;

        let (cookies, user_id) = commenter(app).await;
        let item_id = create_item(app).await;
        seed_history(app, user_id, item_id, 5).await;

        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Pending.as_i16()),
            "with the ladder off, even an established account waits"
        );

        clear_settings(app).await;
    });
}

/// On a site that publishes immediately the ladder is irrelevant, and must not
/// start holding anything.
#[test]
fn the_ladder_does_not_hold_comments_on_an_open_site() {
    run_test(async {
        let app = shared_app().await;
        let _guard = SETTINGS.lock().await;
        ensure_item_type(app).await;
        set_settings(app, Some("published"), Some(3)).await;

        let (cookies, _) = commenter(app).await;
        let item_id = create_item(app).await;

        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Published.as_i16()),
            "a brand new account still publishes on a site that holds nothing"
        );

        clear_settings(app).await;
    });
}

/// The default threshold applies when nothing is configured, so the ladder works
/// out of the box on a moderated site.
#[test]
fn the_default_threshold_applies_when_unset() {
    run_test(async {
        let app = shared_app().await;
        let _guard = SETTINGS.lock().await;
        ensure_item_type(app).await;
        set_settings(app, Some("pending"), None).await;

        let (cookies, user_id) = commenter(app).await;
        let item_id = create_item(app).await;
        seed_history(app, user_id, item_id, 3).await;

        assert_eq!(
            post_comment_status(app, &cookies, item_id).await,
            i64::from(CommentStatus::Published.as_i16()),
            "three approved comments is the default threshold"
        );

        clear_settings(app).await;
    });
}
