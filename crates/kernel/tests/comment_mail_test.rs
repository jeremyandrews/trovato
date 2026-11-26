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
