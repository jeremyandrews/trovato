#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Comment notifications go through the mail outbox (BG18).
//!
//! The notification to a post's author is queued in the same transaction as
//! the moderation update that approves a held comment, and approving the same
//! comment twice queues one message. (That posting a held comment queues
//! nothing is the rule `should_notify_on_publish`, unit tested in
//! `routes/comment.rs`; here the held comment is made directly.) Delivered over
//! real SMTP to a loopback sink. Requires Postgres and Redis.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::smtp_sink::SmtpSink;
use common::{TestApp, extract_cookies, run_test};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CommentStatus, CreateComment, CreateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const ITEM_TYPE: &str = "comment_mail_test";

/// `comment_notify_authors` is one row of site-wide configuration: every test
/// here that depends on it, or changes it, holds this.
static NOTICES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A page's CSRF token, carrying the session cookies forward.
async fn csrf(app: &TestApp, cookies: &str, page: &str) -> (String, String) {
    let response = app
        .request_with_cookies(Request::get(page).body(Body::empty()).unwrap(), cookies)
        .await;
    let fresh = extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let html = text(response).await;
    let token = ["name=\"csrf-token\" content=\"", "name=\"_token\" value=\""]
        .iter()
        .find_map(|marker| {
            html.find(marker).map(|start| {
                html[start + marker.len()..]
                    .split('"')
                    .next()
                    .unwrap()
                    .to_string()
            })
        })
        .expect("a CSRF token on the page");
    (cookies, token)
}

async fn setup(app: &TestApp) {
    app.ensure_plugin_enabled("trovato_comments").await;
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Comment Mail Test', 'Fixture type', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(
            ITEM_TYPE,
            "Comment Mail Test",
            Some("Fixture type"),
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();
}

/// A new member with an address; returns their id and address.
async fn member(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let mail = format!("{name}@bg18.example.test");
    app.create_test_user(&name, "Bg18-Password-123", &mail)
        .await;
    (common::user_id_of(app, &name).await, mail)
}

async fn queued_for(app: &TestApp, comment: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE dedupe_key = $1")
        .bind(format!("comment-published:{comment}"))
        .fetch_one(&app.db)
        .await
        .unwrap()
}

#[test]
fn a_held_comment_notifies_once_when_approved_and_never_before() {
    run_test(async {
        // It needs the author notice on (B183).
        let _notices = NOTICES.lock().await;
        let sink = SmtpSink::start().await;
        let app = TestApp::with_config(|config| {
            config.smtp_host = Some("127.0.0.1".to_string());
            config.smtp_port = sink.port;
            config.smtp_encryption = "none".to_string();
        })
        .await;
        setup(&app).await;
        let system = UserContext::administrator(Uuid::nil(), vec![]);

        let (author, author_mail) = member(&app, "bg18author").await;
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: ITEM_TYPE.to_string(),
                    title: format!("Comment mail {}", Uuid::now_v7().simple()),
                    author_id: author,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: None,
                },
                &system,
            )
            .await
            .unwrap()
            .id;

        // The held comment is made directly: the site's default comment status
        // is one row every test binary shares, so this test leaves it alone.
        let (commenter, _) = member(&app, "bg18commenter").await;
        let comment = app
            .state
            .comments()
            .create(
                CreateComment {
                    item_id: item,
                    parent_id: None,
                    author_id: commenter,
                    subject: Some("BG18 held comment subject".to_string()),
                    body: "Held for review, BG18".to_string(),
                    body_format: None,
                    status: Some(CommentStatus::Pending.as_i16()),
                },
                &system,
            )
            .await
            .unwrap()
            .id;
        assert_eq!(
            queued_for(&app, comment).await,
            0,
            "nothing is queued for the comment before it is approved"
        );

        // An administrator approves it, unpublishes it and approves it again:
        // the rule says notify on both approvals, and the dedupe key holds
        // it to one message.
        let admin = common::username("bg18admin");
        let admin_cookies = app
            .create_and_login_admin(
                &admin,
                "Bg18-Password-123",
                &format!("{admin}@bg18.example.test"),
            )
            .await;
        for action in ["approve", "unpublish", "approve"] {
            let (cookies, token) = csrf(&app, &admin_cookies, "/admin/content/comments").await;
            let response = app
                .request_with_cookies(
                    Request::post(format!("/admin/content/comments/{comment}/{action}"))
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(format!(
                            "_token={}",
                            urlencoding::encode(&token)
                        )))
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert!(response.status().is_redirection(), "{}", response.status());
        }
        assert_eq!(
            queued_for(&app, comment).await,
            1,
            "approved twice, queued once"
        );

        let stats = app.deliver_mail_to(&[&author_mail]).await;
        assert_eq!(stats.sent, 1, "{stats:?}");
        let messages: Vec<_> = sink
            .messages()
            .into_iter()
            .filter(|m| m.recipients.contains(&author_mail))
            .collect();
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0].data.contains("Held for review, BG18"),
            "the notification quotes the comment"
        );
        assert!(
            messages[0]
                .data
                .contains("Subject: BG18 held comment subject"),
            "the comment's own subject is the mail's subject (B163)"
        );
        sink.stop().await;
    });
}

/// Set `comment_notify_authors`, or remove it (`None`, the default: on).
async fn set_notices(app: &TestApp, value: Option<bool>) {
    match value {
        Some(on) => trovato_kernel::models::SiteConfig::set(
            &app.db,
            trovato_kernel::models::comment::NOTIFY_AUTHORS_KEY,
            serde_json::Value::Bool(on),
        )
        .await
        .expect("set the notice setting"),
        None => {
            sqlx::query("DELETE FROM site_config WHERE key = $1")
                .bind(trovato_kernel::models::comment::NOTIFY_AUTHORS_KEY)
                .execute(&app.db)
                .await
                .expect("clear the notice setting");
        }
    }
}

/// Delete, when the test ends, the accounts `users` (with their own roles),
/// the items `items` with their comments, and the mail queued for `mail`.
fn clean_up_on_exit(app: &TestApp, users: Vec<Uuid>, items: Vec<Uuid>, mail: Vec<String>) {
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        for recipient in &mail {
            sqlx::query("DELETE FROM mail_outbox WHERE recipient = $1")
                .bind(recipient)
                .execute(&db)
                .await
                .expect("delete queued mail");
        }
        for item in &items {
            sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(item)
                .execute(&db)
                .await
                .expect("delete the item and its comments");
        }
        for user in &users {
            let roles: Vec<Uuid> =
                sqlx::query_scalar("SELECT role_id FROM user_roles WHERE user_id = $1")
                    .bind(user)
                    .fetch_all(&db)
                    .await
                    .unwrap_or_default();
            for statement in [
                "DELETE FROM comment WHERE author_id = $1",
                "DELETE FROM user_roles WHERE user_id = $1",
                "DELETE FROM users WHERE id = $1",
            ] {
                sqlx::query(statement)
                    .bind(user)
                    .execute(&db)
                    .await
                    .expect(statement);
            }
            for role in roles {
                for statement in [
                    "DELETE FROM role_permissions WHERE role_id = $1",
                    "DELETE FROM roles WHERE id = $1 AND name LIKE 'b183%'",
                ] {
                    sqlx::query(statement)
                        .bind(role)
                        .execute(&db)
                        .await
                        .expect(statement);
                }
            }
        }
    });
}

/// B183: "E-mail content authors about new comments". On (the default), a
/// comment posted visible and a held comment approved each queue the author's
/// notice; off, neither does. Both through the routes a site uses: the
/// comment route and the moderation screen's Approve.
#[test]
fn the_author_notice_follows_the_site_setting_on_both_paths() {
    run_test(async {
        let notices = NOTICES.lock().await;
        common::hold_until_cleanup(notices);
        let sink = SmtpSink::start().await;
        let app = TestApp::with_config(|config| {
            config.smtp_host = Some("127.0.0.1".to_string());
            config.smtp_port = sink.port;
            config.smtp_encryption = "none".to_string();
        })
        .await;
        setup(&app).await;
        let system = UserContext::administrator(Uuid::nil(), vec![]);

        let (author, author_mail) = member(&app, "b183author").await;
        let (commenter, commenter_mail) = member(&app, "b183commenter").await;
        let admin = common::username("b183admin");
        let admin_cookies = app
            .create_and_login_admin(
                &admin,
                "B183-Password-123",
                &format!("{admin}@bg18.example.test"),
            )
            .await;
        let admin_id = common::user_id_of(&app, &admin).await;
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: ITEM_TYPE.to_string(),
                    title: format!("Comment notice {}", Uuid::now_v7().simple()),
                    author_id: author,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: None,
                },
                &system,
            )
            .await
            .unwrap()
            .id;
        clean_up_on_exit(
            &app,
            vec![commenter, admin_id, author],
            vec![item],
            vec![author_mail.clone(), commenter_mail],
        );
        // Put the setting back as the site had it.
        let before = trovato_kernel::models::SiteConfig::get(
            &app.db,
            trovato_kernel::models::comment::NOTIFY_AUTHORS_KEY,
        )
        .await
        .unwrap()
        .and_then(|v| v.as_bool());
        {
            let db = app.db.clone();
            common::defer_cleanup(move || async move {
                let key = trovato_kernel::models::comment::NOTIFY_AUTHORS_KEY;
                match before {
                    Some(on) => trovato_kernel::models::SiteConfig::set(
                        &db,
                        key,
                        serde_json::Value::Bool(on),
                    )
                    .await
                    .expect("restore the notice setting"),
                    None => {
                        sqlx::query("DELETE FROM site_config WHERE key = $1")
                            .bind(key)
                            .execute(&db)
                            .await
                            .expect("clear the notice setting");
                    }
                }
            });
        }

        // The commenter may post, straight to published whatever the site's
        // default status is (one row other binaries change).
        let role = common::create_test_role(&app, &common::username("b183role")).await;
        for permission in ["post comments", "access content", "skip comment approval"] {
            sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2)")
                .bind(role)
                .bind(permission)
                .execute(&app.db)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2)")
            .bind(commenter)
            .bind(role)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.permissions().invalidate_all();
        let commenter_name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(commenter)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let commenter_cookies = app.login(&commenter_name, "Bg18-Password-123").await;

        for (setting, expected) in [(Some(false), 0), (None, 1), (Some(true), 1)] {
            set_notices(&app, setting).await;

            // Posted visible through the comment route.
            let (cookies, token) = csrf(&app, &commenter_cookies, "/").await;
            let response = app
                .request_with_cookies(
                    Request::post(format!("/api/v1/item/{item}/comments"))
                        .header("content-type", "application/json")
                        .header("x-csrf-token", &token)
                        .body(Body::from(
                            serde_json::json!({ "body": format!("B183 posted, {setting:?}") })
                                .to_string(),
                        ))
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(response.status(), 200, "post with {setting:?}");
            let posted: serde_json::Value = serde_json::from_str(&text(response).await).unwrap();
            assert_eq!(posted["status"], CommentStatus::Published.as_i16());
            let posted = posted["id"].as_str().unwrap().parse::<Uuid>().unwrap();
            assert_eq!(
                queued_for(&app, posted).await,
                expected,
                "posting with the setting {setting:?}"
            );

            // Held, then approved on the moderation screen.
            let held = app
                .state
                .comments()
                .create(
                    CreateComment {
                        item_id: item,
                        parent_id: None,
                        author_id: commenter,
                        subject: None,
                        body: format!("B183 held, {setting:?}"),
                        body_format: None,
                        status: Some(CommentStatus::Pending.as_i16()),
                    },
                    &system,
                )
                .await
                .unwrap()
                .id;
            let (cookies, token) = csrf(&app, &admin_cookies, "/admin/content/comments").await;
            let response = app
                .request_with_cookies(
                    Request::post(format!("/admin/content/comments/{held}/approve"))
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(format!(
                            "_token={}",
                            urlencoding::encode(&token)
                        )))
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert!(response.status().is_redirection(), "{}", response.status());
            assert_eq!(
                queued_for(&app, held).await,
                expected,
                "approving with the setting {setting:?}"
            );
        }

        // The settings screen shows the setting, ticked while it is on.
        let response = app
            .request_with_cookies(
                Request::get("/admin/content/comments")
                    .body(Body::empty())
                    .unwrap(),
                &admin_cookies,
            )
            .await;
        let page = text(response).await;
        assert!(
            page.contains("E-mail content authors about new comments"),
            "the setting is on the comment settings form"
        );
        assert!(
            page.contains(r#"name="notify_authors" value="1" class="form-checkbox" checked"#),
            "and ticked while on"
        );
        sink.stop().await;
    });
}
