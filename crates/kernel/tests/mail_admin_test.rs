#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The mail log pages and the bounce webhook (BG18 part 2,
//! `docs/design/mail-queue.md` §4 and §9).
//!
//! The webhook takes only signed, fresh, unseen Mailgun events; a hard bounce
//! fails the message it names and stops later mail to the address, a
//! complaint stops bulk mail only. The network pages take `administer site`
//! and every action a CSRF token; the blog owner page shows its own blog and
//! nothing else, and only to an active member holding `view own blog mail`.
//!
//! One app with mail and a signing key, over a loopback SMTP sink, serves the
//! file. Each test uses addresses of its own and drains only those, because
//! the outbox is shared by every test using the database. Requires Postgres
//! and Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::smtp_sink::SmtpSink;
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::PgPool;
use trovato_kernel::config::WebhookKey;
use trovato_kernel::models::DEFAULT_TENANT_ID;
use trovato_kernel::services::email::{MailSecret, NewMail, SECRET_MARKER};
use uuid::Uuid;

const KEY: &str = "bg18-test-signing-key";
const FROM: &str = "noreply@bg18-admin.example.test";

static APP: std::sync::OnceLock<(SmtpSink, TestApp)> = std::sync::OnceLock::new();

/// The file's app: mail over the sink, and the webhook signing key set.
fn app() -> &'static TestApp {
    &APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || {
            handle.block_on(async {
                let sink = SmtpSink::start().await;
                let port = sink.port;
                let app = TestApp::with_config(|config| {
                    config.smtp_host = Some("127.0.0.1".to_string());
                    config.smtp_port = port;
                    config.smtp_encryption = "none".to_string();
                    config.smtp_from_email = FROM.to_string();
                    config.mail.webhook_signing_key = Some(WebhookKey(KEY.to_string()));
                })
                .await;
                (sink, app)
            })
        })
        .join()
        .expect("mail admin app init thread panicked")
    })
    .1
}

fn sink() -> &'static SmtpSink {
    &APP.get().expect("app() first").0
}

/// A unique address for one test.
fn address(tag: &str) -> String {
    format!("{tag}-{}@bg18-admin.example.test", Uuid::now_v7().simple())
}

/// Remove this address's outbox rows and suppression after the test.
fn clean_up(pool: &PgPool, address: &str) {
    let pool = pool.clone();
    let address = address.to_string();
    defer_cleanup(move || async move {
        sqlx::query("DELETE FROM mail_suppression WHERE address = $1")
            .bind(&address)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM mail_outbox WHERE recipient = $1")
            .bind(&address)
            .execute(&pool)
            .await
            .ok();
    });
}

/// Queue one message to `to` and return its id.
async fn queue(app: &TestApp, mail: NewMail) -> i64 {
    app.state
        .email()
        .unwrap()
        .queue_now(mail)
        .await
        .unwrap()
        .expect("queued")
}

/// Queue a notification to `to` and send it through the sink.
async fn sent_to(app: &TestApp, to: &str) -> i64 {
    let id = queue(
        app,
        NewMail::notification("test", to, "BG18 admin test", "Hello"),
    )
    .await;
    let stats = app.deliver_mail_to(&[to]).await;
    assert_eq!(stats.sent, 1, "{stats:?}");
    id
}

/// `(status, attempts, last_error)` of one row.
async fn row(pool: &PgPool, id: i64) -> (String, i32, Option<String>) {
    sqlx::query_as("SELECT status, attempts, last_error FROM mail_outbox WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The suppression reason recorded for `address`, if any.
async fn suppression(pool: &PgPool, address: &str) -> Option<String> {
    sqlx::query_scalar("SELECT reason FROM mail_suppression WHERE address = $1")
        .bind(address)
        .fetch_optional(pool)
        .await
        .unwrap()
}

fn sign(timestamp: &str, token: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(token.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// A Mailgun webhook body for `event`, signed now with a fresh token.
fn mailgun_event(event: serde_json::Value) -> serde_json::Value {
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let token = Uuid::now_v7().simple().to_string();
    serde_json::json!({
        "signature": {
            "timestamp": timestamp,
            "token": token,
            "signature": sign(&timestamp, &token),
        },
        "event-data": event,
    })
}

fn hard_bounce(to: &str, id: i64) -> serde_json::Value {
    mailgun_event(serde_json::json!({
        "event": "failed",
        "severity": "permanent",
        "recipient": to,
        "reason": "bounce",
        "delivery-status": {"code": 550, "message": "5.1.1 The email account does not exist"},
        "message": {"headers": {"message-id": format!("outbox-{id}@bg18-admin.example.test")}},
    }))
}

fn complaint(to: &str, id: i64) -> serde_json::Value {
    mailgun_event(serde_json::json!({
        "event": "complained",
        "recipient": to,
        "message": {"headers": {"message-id": format!("outbox-{id}@bg18-admin.example.test")}},
    }))
}

async fn post_webhook(app: &TestApp, body: &serde_json::Value) -> StatusCode {
    app.request(
        Request::post("/mail/webhook/mailgun")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .status()
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn post_form(app: &TestApp, path: &str, cookies: &str, form: &str) -> StatusCode {
    app.request_with_cookies(
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form.to_string()))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// The CSRF token a page rendered into its forms.
fn csrf_from(html: &str) -> String {
    let at = html.find("name=\"_token\" value=\"").expect("a form token") + 21;
    html[at..at + html[at..].find('"').unwrap()].to_string()
}

/// A fresh CSRF token from `path`: tokens are single use.
async fn token(app: &TestApp, path: &str, cookies: &str) -> String {
    csrf_from(&get(app, path, cookies).await.1)
}

/// An administrator's session cookies.
async fn admin(app: &TestApp) -> String {
    let name = common::username("bg18mailadmin");
    app.create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await
}

// --- the webhook -----------------------------------------------------------

/// A hard bounce fails the message it names, suppresses the address network
/// wide, and the next message to it is suppressed, not sent.
#[test]
fn a_hard_bounce_suppresses_later_mail() {
    run_test(async {
        let app = app();
        let to = address("hardbounce");
        clean_up(&app.db, &to);
        let first = sent_to(app, &to).await;

        assert_eq!(
            post_webhook(app, &hard_bounce(&to, first)).await,
            StatusCode::OK
        );
        let (status, _, error) = row(&app.db, first).await;
        assert_eq!(status, "failed");
        assert_eq!(
            error.as_deref(),
            Some("hard bounce: 550 5.1.1 The email account does not exist")
        );
        let bounced: bool =
            sqlx::query_scalar("SELECT bounced_at IS NOT NULL FROM mail_outbox WHERE id = $1")
                .bind(first)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(bounced);
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("hard_bounce")
        );

        // Account mail too: the address does not exist.
        let second = queue(app, NewMail::account("test", &to, "Again", "Hello")).await;
        let stats = app.deliver_mail_to(&[&to]).await;
        assert_eq!((stats.sent, stats.suppressed), (0, 1), "{stats:?}");
        assert_eq!(row(&app.db, second).await.0, "suppressed");
        let delivered = sink()
            .messages()
            .into_iter()
            .filter(|m| m.recipients.contains(&to))
            .count();
        assert_eq!(delivered, 1, "only the first message reached the sink");
    });
}

/// A complaint stops bulk mail only: the member can still reset a password.
#[test]
fn a_complaint_stops_bulk_mail_only() {
    run_test(async {
        let app = app();
        let to = address("complaint");
        clean_up(&app.db, &to);
        let first = sent_to(app, &to).await;

        assert_eq!(
            post_webhook(app, &complaint(&to, first)).await,
            StatusCode::OK
        );
        assert_eq!(row(&app.db, first).await.0, "sent");
        let complained: bool =
            sqlx::query_scalar("SELECT complained_at IS NOT NULL FROM mail_outbox WHERE id = $1")
                .bind(first)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(complained);
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("complaint")
        );

        let bulk = queue(
            app,
            NewMail::bulk("digest", &to, "Digest", "News", "https://x.test/u"),
        )
        .await;
        let account = queue(app, NewMail::account("test", &to, "Reset", "Hello")).await;
        let stats = app.deliver_mail_to(&[&to]).await;
        assert_eq!((stats.sent, stats.suppressed), (1, 1), "{stats:?}");
        assert_eq!(row(&app.db, bulk).await.0, "suppressed");
        assert_eq!(row(&app.db, account).await.0, "sent");

        // A later hard bounce stops everything; a later complaint does not
        // weaken it back.
        assert_eq!(
            post_webhook(app, &hard_bounce(&to, account)).await,
            StatusCode::OK
        );
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("hard_bounce")
        );
        assert_eq!(
            post_webhook(app, &complaint(&to, account)).await,
            StatusCode::OK
        );
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("hard_bounce")
        );
    });
}

/// Forged, stale and replayed requests change nothing.
#[test]
fn unsigned_stale_and_replayed_events_are_refused() {
    run_test(async {
        let app = app();
        let to = address("forged");
        clean_up(&app.db, &to);
        let id = sent_to(app, &to).await;

        let mut forged = hard_bounce(&to, id);
        forged["signature"]["signature"] = serde_json::json!(sign("1", "2"));
        assert_eq!(post_webhook(app, &forged).await, StatusCode::UNAUTHORIZED);

        let mut tampered = hard_bounce(&to, id);
        tampered["signature"]["token"] = serde_json::json!("another-token");
        assert_eq!(post_webhook(app, &tampered).await, StatusCode::UNAUTHORIZED);

        let old = (chrono::Utc::now().timestamp() - 301).to_string();
        let mut stale = hard_bounce(&to, id);
        stale["signature"] = serde_json::json!({
            "timestamp": old, "token": "stale-token", "signature": sign(&old, "stale-token"),
        });
        assert_eq!(post_webhook(app, &stale).await, StatusCode::UNAUTHORIZED);
        assert_eq!(row(&app.db, id).await.0, "sent");
        assert_eq!(suppression(&app.db, &to).await, None);

        // A genuine event, then the same request again after the address was
        // removed from the list: the replay is acknowledged and ignored.
        let genuine = hard_bounce(&to, id);
        assert_eq!(post_webhook(app, &genuine).await, StatusCode::OK);
        sqlx::query("DELETE FROM mail_suppression WHERE address = $1")
            .bind(&to)
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(post_webhook(app, &genuine).await, StatusCode::OK);
        assert_eq!(suppression(&app.db, &to).await, None, "a replay was taken");

        assert_eq!(
            post_webhook(app, &serde_json::json!({"not": "mailgun"})).await,
            StatusCode::NOT_ACCEPTABLE
        );
    });
}

/// Events the kernel does not act on are acknowledged and change nothing; a
/// bounce naming a row sent to someone else does not touch that row.
#[test]
fn other_events_and_other_rows_are_left_alone() {
    run_test(async {
        let app = app();
        let to = address("other");
        let someone = address("someone");
        clean_up(&app.db, &to);
        clean_up(&app.db, &someone);
        let theirs = sent_to(app, &someone).await;

        let temporary = mailgun_event(serde_json::json!({
            "event": "failed", "severity": "temporary", "recipient": to,
        }));
        assert_eq!(post_webhook(app, &temporary).await, StatusCode::OK);
        let delivered = mailgun_event(serde_json::json!({"event": "delivered", "recipient": to}));
        assert_eq!(post_webhook(app, &delivered).await, StatusCode::OK);
        assert_eq!(suppression(&app.db, &to).await, None);

        // A bounce for `to` that names `someone`'s message (another install on
        // the same sending domain): `to` is suppressed, the row is not touched.
        assert_eq!(
            post_webhook(app, &hard_bounce(&to, theirs)).await,
            StatusCode::OK
        );
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("hard_bounce")
        );
        assert_eq!(row(&app.db, theirs).await.0, "sent");
        assert_eq!(suppression(&app.db, &someone).await, None);
    });
}

/// Suppression is keyed by the lower-cased address: a bounce reported in any
/// case matches the row and stops later mail to a mixed case recipient.
#[test]
fn a_bounce_for_a_mixed_case_address_suppresses_it() {
    run_test(async {
        let app = app();
        let to = address("MixedCase");
        let lower = to.to_lowercase();
        clean_up(&app.db, &to);
        clean_up(&app.db, &lower);
        let first = sent_to(app, &to).await;

        assert_eq!(
            post_webhook(app, &hard_bounce(&to.to_uppercase(), first)).await,
            StatusCode::OK
        );
        assert_eq!(row(&app.db, first).await.0, "failed");
        assert_eq!(
            suppression(&app.db, &lower).await.as_deref(),
            Some("hard_bounce")
        );
        let second = queue(app, NewMail::account("test", &to, "Again", "Hello")).await;
        let stats = app.deliver_mail_to(&[&to]).await;
        assert_eq!((stats.sent, stats.suppressed), (0, 1), "{stats:?}");
        assert_eq!(row(&app.db, second).await.0, "suppressed");

        // The list's recipient filter is case insensitive too.
        let cookies = admin(app).await;
        let (_, html) = get(
            app,
            &format!(
                "/admin/reports/mail?recipient={}",
                urlencoding::encode(&lower)
            ),
            &cookies,
        )
        .await;
        assert!(html.contains(&format!("/admin/reports/mail/{first}\"")));
    });
}

/// With no signing key, or no mail at all, there is no webhook.
#[test]
fn without_a_key_the_webhook_does_not_exist() {
    run_test(async {
        let body = hard_bounce("nobody@bg18-admin.example.test", 1);
        let plain = shared_app().await;
        assert!(
            plain.state.email().is_none(),
            "the shared app is expected to have no mail"
        );
        assert_eq!(post_webhook(plain, &body).await, StatusCode::NOT_FOUND);

        // Mail configured, no key: still no webhook, even for a request signed
        // with the empty key. (A blank `MAIL_WEBHOOK_SIGNING_KEY` resolving to no
        // key is pinned in `config_tests`.)
        let keyless = TestApp::with_config(|config| {
            config.smtp_host = Some("127.0.0.1".to_string());
            config.smtp_port = 2525;
            config.smtp_encryption = "none".to_string();
            config.smtp_from_email = FROM.to_string();
            config.mail.webhook_signing_key = None;
        })
        .await;
        assert!(keyless.state.email().is_some());
        assert_eq!(post_webhook(&keyless, &body).await, StatusCode::NOT_FOUND);
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"").unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b"empty-key-token");
        let empty_key = serde_json::json!({
            "signature": {"timestamp": timestamp, "token": "empty-key-token",
                          "signature": hex::encode(mac.finalize().into_bytes())},
            "event-data": {"event": "failed", "severity": "permanent",
                           "recipient": "nobody@bg18-admin.example.test"},
        });
        assert_eq!(
            post_webhook(&keyless, &empty_key).await,
            StatusCode::NOT_FOUND
        );
    });
}

// --- the network pages -----------------------------------------------------

/// Every network page and action takes `administer site`: refused to the
/// anonymous, to a member without it, and to a blog owner.
#[test]
fn the_network_pages_take_administer_site() {
    run_test(async {
        let app = app();
        let to = address("gate");
        clean_up(&app.db, &to);
        let id = queue(app, NewMail::notification("test", &to, "Gate", "Hello")).await;
        let (_, member) = user_holding(app, "bg18mailmember", &[]).await;
        let (_, owner) = user_holding(app, "bg18mailowner", &["view own blog mail"]).await;
        let pages = [
            "/admin/reports/mail".to_string(),
            format!("/admin/reports/mail/{id}"),
            "/admin/reports/mail/suppressions".to_string(),
        ];
        for path in &pages {
            let (status, _) = get(app, path, "").await;
            assert_eq!(status, StatusCode::SEE_OTHER, "anonymous {path}");
            for cookies in [&member, &owner] {
                let (status, _) = get(app, path, cookies).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
            }
        }
        // Actions too, before the token is even read.
        for path in [
            format!("/admin/reports/mail/{id}/cancel"),
            format!("/admin/reports/mail/{id}/retry"),
            "/admin/reports/mail/suppressions/remove".to_string(),
            "/admin/reports/mail/suppressions".to_string(),
        ] {
            let status = post_form(app, &path, &owner, &format!("_token=x&address={to}")).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        }
        assert_eq!(row(&app.db, id).await.0, "queued");
    });
}

/// The list filters by recipient and status and counts by status; the detail
/// page shows the headers and the body with the one time secret shown as a
/// placeholder, never the marker or a token.
#[test]
fn the_list_and_the_detail_show_what_was_queued() {
    run_test(async {
        let app = app();
        let cookies = admin(app).await;
        let to = address("listed");
        clean_up(&app.db, &to);
        let sent = sent_to(app, &to).await;
        let reset = queue(
            app,
            NewMail::account(
                "password_reset",
                &to,
                "Reset your password",
                &format!("Reset: https://x.test/user/password-reset/{SECRET_MARKER}"),
            )
            .secret(MailSecret::PasswordReset {
                user_id: Uuid::now_v7(),
            }),
        )
        .await;

        let (status, html) = get(
            app,
            &format!("/admin/reports/mail?recipient={}", urlencoding::encode(&to)),
            &cookies,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!("/admin/reports/mail/{sent}\"")),
            "{html}"
        );
        assert!(html.contains(&format!("/admin/reports/mail/{reset}\"")));
        assert!(html.contains("All (2)"));
        assert!(html.contains("Queued (1)"));
        assert!(html.contains("Sent (1)"));

        let (_, html) = get(
            app,
            &format!(
                "/admin/reports/mail?status=sent&recipient={}",
                urlencoding::encode(&to)
            ),
            &cookies,
        )
        .await;
        assert!(html.contains(&format!("/admin/reports/mail/{sent}\"")));
        assert!(!html.contains(&format!("/admin/reports/mail/{reset}\"")));
        assert!(
            html.contains("All (2)") && html.contains("Queued (1)"),
            "the status counts took the status filter"
        );

        let filtered = |extra: &str| {
            format!(
                "/admin/reports/mail?recipient={}&{extra}",
                urlencoding::encode(&to)
            )
        };
        let (_, html) = get(app, &filtered("category=password_reset"), &cookies).await;
        assert!(html.contains(&format!("/admin/reports/mail/{reset}\"")));
        assert!(!html.contains(&format!("/admin/reports/mail/{sent}\"")));
        assert!(
            html.contains("All (1)"),
            "the counts ignored the category filter"
        );
        let (_, html) = get(app, &filtered("kind=bulk"), &cookies).await;
        assert!(!html.contains(&format!("/admin/reports/mail/{sent}\"")));
        assert!(!html.contains(&format!("/admin/reports/mail/{reset}\"")));
        assert!(
            html.contains("All (0)"),
            "the counts ignored the kind filter"
        );
        let (_, html) = get(app, &filtered("kind=transactional"), &cookies).await;
        assert!(html.contains("All (2)"));

        let (status, html) = get(app, &format!("/admin/reports/mail/{reset}"), &cookies).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Reset your password"));
        assert!(html.contains(&format!("&lt;outbox-{reset}@bg18-admin.example.test&gt;")));
        assert!(html.contains("[one time secret, minted when sent]"));
        assert!(!html.contains(SECRET_MARKER));
        assert!(html.contains("Cancel this message"));
        assert!(!html.contains(">Retry<"));

        let (status, _) = get(app, "/admin/reports/mail/999999999999", &cookies).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    });
}

/// Cancel takes a queued message out of the queue; Retry puts a failed one
/// back with its attempts reset. Without the session's token neither does
/// anything, and neither applies to a message in the wrong state.
#[test]
fn retry_and_cancel_need_a_token_and_the_right_state() {
    run_test(async {
        let app = app();
        let cookies = admin(app).await;
        let to = address("actions");
        clean_up(&app.db, &to);
        let id = queue(app, NewMail::notification("test", &to, "Act", "Hello")).await;
        // A token is the session's, not the form's: the suppression page has
        // a form whatever state the message is in.
        let tokens = "/admin/reports/mail/suppressions";

        let cancel = format!("/admin/reports/mail/{id}/cancel");
        let retry = format!("/admin/reports/mail/{id}/retry");
        assert_eq!(
            post_form(app, &cancel, &cookies, "_token=wrong").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(row(&app.db, id).await.0, "queued");
        // A queued message cannot be retried.
        assert_eq!(
            post_form(
                app,
                &retry,
                &cookies,
                &format!("_token={}", token(app, tokens, &cookies).await)
            )
            .await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(row(&app.db, id).await.0, "queued");

        assert_eq!(
            post_form(
                app,
                &cancel,
                &cookies,
                &format!("_token={}", token(app, tokens, &cookies).await)
            )
            .await,
            StatusCode::SEE_OTHER
        );
        let (status, _, error) = row(&app.db, id).await;
        assert_eq!(status, "cancelled");
        assert_eq!(error.as_deref(), Some("cancelled by an administrator"));
        let (_, html) = get(app, &format!("/admin/reports/mail/{id}"), &cookies).await;
        assert!(html.contains("The message is cancelled."));
        assert!(html.contains(">Retry<"));
        // Nothing is sent once cancelled.
        assert_eq!(app.deliver_mail_to(&[&to]).await.sent, 0);

        // Retry takes a cancelled message back too, not only a failed one.
        assert_eq!(
            post_form(
                app,
                &retry,
                &cookies,
                &format!("_token={}", token(app, tokens, &cookies).await)
            )
            .await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(row(&app.db, id).await, ("queued".to_string(), 0, None));

        // Fail it as a worker would, then retry.
        sqlx::query(
            "UPDATE mail_outbox SET status = 'failed', attempts = 12, \
             last_error = 'refused', finished_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&app.db)
        .await
        .unwrap();
        assert_eq!(
            post_form(app, &retry, &cookies, "_token=wrong").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(row(&app.db, id).await.0, "failed");
        assert_eq!(
            post_form(
                app,
                &retry,
                &cookies,
                &format!("_token={}", token(app, tokens, &cookies).await)
            )
            .await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(row(&app.db, id).await, ("queued".to_string(), 0, None));
        let stats = app.deliver_mail_to(&[&to]).await;
        assert_eq!(stats.sent, 1, "{stats:?}");
        assert_eq!(row(&app.db, id).await.0, "sent");

        // A sent message can be neither.
        for path in [&cancel, &retry] {
            assert_eq!(
                post_form(
                    app,
                    path,
                    &cookies,
                    &format!("_token={}", token(app, tokens, &cookies).await),
                )
                .await,
                StatusCode::SEE_OTHER
            );
        }
        assert_eq!(row(&app.db, id).await.0, "sent");
        let (_, html) = get(app, &format!("/admin/reports/mail/{id}"), &cookies).await;
        assert!(html.contains("This message cannot be retried"), "{html}");

        // Nor one a worker holds.
        sqlx::query(
            "UPDATE mail_outbox SET status = 'sending', locked_by = 'bg18-test', \
             locked_until = now() + interval '10 minutes', finished_at = NULL WHERE id = $1",
        )
        .bind(id)
        .execute(&app.db)
        .await
        .unwrap();
        for path in [&cancel, &retry] {
            assert_eq!(
                post_form(
                    app,
                    path,
                    &cookies,
                    &format!("_token={}", token(app, tokens, &cookies).await),
                )
                .await,
                StatusCode::SEE_OTHER
            );
        }
        assert_eq!(
            row(&app.db, id).await.0,
            "sending",
            "an action touched a row a worker holds"
        );

        // Nor can one whose body retention cleared.
        sqlx::query(
            "UPDATE mail_outbox SET status = 'failed', text_body = NULL, \
             body_purged_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&app.db)
        .await
        .unwrap();
        assert_eq!(
            post_form(
                app,
                &retry,
                &cookies,
                &format!("_token={}", token(app, tokens, &cookies).await),
            )
            .await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(row(&app.db, id).await.0, "failed");
        let (_, html) = get(app, &format!("/admin/reports/mail/{id}"), &cookies).await;
        assert!(html.contains("Cleared by retention"));
        assert!(!html.contains(">Retry<"));
    });
}

/// The suppression list adds and removes addresses, with the token only.
#[test]
fn suppressions_are_added_and_removed_with_a_token() {
    run_test(async {
        let app = app();
        let cookies = admin(app).await;
        let to = address("suppress");
        clean_up(&app.db, &to);
        let add = "/admin/reports/mail/suppressions";
        let (status, _) = get(app, add, &cookies).await;
        assert_eq!(status, StatusCode::OK);
        let remove = "/admin/reports/mail/suppressions/remove";
        let encoded = urlencoding::encode(&to);

        assert_eq!(
            post_form(
                app,
                add,
                &cookies,
                &format!("_token=wrong&address={encoded}")
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(suppression(&app.db, &to).await, None);
        // A valid token, so it is the address check that refuses.
        clean_up(&app.db, "not-an-address");
        assert_eq!(
            post_form(
                app,
                add,
                &cookies,
                &format!(
                    "_token={}&address=not-an-address",
                    token(app, add, &cookies).await
                )
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(suppression(&app.db, "not-an-address").await, None);

        // A complaint stops bulk mail only; adding the address by hand makes
        // it stop everything.
        let complained = sent_to(app, &to).await;
        assert_eq!(
            post_webhook(app, &complaint(&to, complained)).await,
            StatusCode::OK
        );
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("complaint")
        );
        assert_eq!(
            post_form(
                app,
                add,
                &cookies,
                &format!(
                    "_token={}&address={}",
                    token(app, add, &cookies).await,
                    encoded.to_uppercase()
                )
            )
            .await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(suppression(&app.db, &to).await.as_deref(), Some("manual"));
        let (_, html) = get(
            app,
            &format!("/admin/reports/mail/suppressions?address={encoded}"),
            &cookies,
        )
        .await;
        assert!(html.contains(&to));
        assert!(html.contains("added by hand"));

        // Suppressed by hand stops account mail too.
        let id = queue(app, NewMail::account("test", &to, "Blocked", "Hello")).await;
        app.deliver_mail_to(&[&to]).await;
        assert_eq!(row(&app.db, id).await.0, "suppressed");

        // No provider event replaces an entry made by hand.
        assert_eq!(
            post_webhook(app, &hard_bounce(&to, complained)).await,
            StatusCode::OK
        );
        assert_eq!(
            post_webhook(app, &complaint(&to, complained)).await,
            StatusCode::OK
        );
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("manual"),
            "a provider event replaced a manual entry"
        );

        assert_eq!(
            post_form(
                app,
                remove,
                &cookies,
                &format!("_token=wrong&address={encoded}")
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(suppression(&app.db, &to).await.as_deref(), Some("manual"));
        assert_eq!(
            post_form(
                app,
                remove,
                &cookies,
                &format!(
                    "_token={}&address={encoded}",
                    token(app, add, &cookies).await
                )
            )
            .await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(suppression(&app.db, &to).await, None);
        let (_, html) = get(app, "/admin/reports/mail/suppressions", &cookies).await;
        assert!(html.contains(&format!("{to} can be mailed again.")));
    });
}

// --- the blog owner page ---------------------------------------------------

/// A blog owner sees their own blog's mail and no other blog's, whatever the
/// query string says, and only as an active member.
#[test]
fn a_blog_owner_sees_their_own_blog_only() {
    run_test(async {
        let app = app();
        let pool = &app.db;
        let other = Uuid::now_v7();
        sqlx::query("INSERT INTO tenant (id, name, machine_name) VALUES ($1, $2, $3)")
            .bind(other)
            .bind(format!("BG18 other blog {}", other.simple()))
            .bind(format!("bg18_other_{}", other.simple()))
            .execute(pool)
            .await
            .unwrap();
        let cleanup_pool = pool.clone();
        defer_cleanup(move || async move {
            sqlx::query("DELETE FROM tenant WHERE id = $1")
                .bind(other)
                .execute(&cleanup_pool)
                .await
                .ok();
        });
        let to = address("owner");
        clean_up(pool, &to);
        let ours = queue(app, NewMail::notification("ours", &to, "Ours", "Hello")).await;
        let theirs = queue(
            app,
            NewMail::notification("theirs", &to, "Theirs", "Hello").tenant(other),
        )
        .await;

        let (owner_id, owner) = user_holding(app, "bg18blogowner", &["view own blog mail"]).await;
        // A member of the other blog, with the permission, but not of this one.
        let (outsider_id, outsider) =
            user_holding(app, "bg18outsider", &["view own blog mail"]).await;
        let (_, member) = user_holding(app, "bg18plainmember", &[]).await;
        for (user, blog) in [(owner_id, DEFAULT_TENANT_ID), (outsider_id, other)] {
            sqlx::query(
                "INSERT INTO user_tenant (user_id, tenant_id, is_active) VALUES ($1, $2, TRUE)",
            )
            .bind(user)
            .bind(blog)
            .execute(pool)
            .await
            .unwrap();
        }
        // The owner sees that a message bounced.
        sqlx::query(
            "UPDATE mail_outbox SET status = 'failed', bounced_at = now(), \
             last_error = 'hard bounce: 550 bg18 test' WHERE id = $1",
        )
        .bind(ours)
        .execute(pool)
        .await
        .unwrap();

        let list = format!("/admin/mail?recipient={}", urlencoding::encode(&to));
        let (status, html) = get(app, &list, &owner).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Ours"), "{html}");
        assert!(!html.contains("Theirs"));
        assert!(html.contains("All (1)"));
        assert!(html.contains("<span class=\"mail-note\">bounced</span>"));
        assert!(html.contains("hard bounce: 550 bg18 test"));
        // No message bodies and no suppression list from here.
        assert!(!html.contains(&format!("/admin/reports/mail/{ours}")));
        assert!(!html.contains("/admin/reports/mail/suppressions"));

        let (_, html) = get(app, &format!("{list}&blog={other}"), &owner).await;
        assert!(html.contains("Ours"));
        assert!(
            !html.contains("Theirs"),
            "the blog filter reached another blog"
        );
        let _ = theirs;

        let (status, _) = get(app, &list, &outsider).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "not a member of the blog");
        let (status, _) = get(app, &list, &member).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "no permission");

        sqlx::query("UPDATE user_tenant SET is_active = FALSE WHERE user_id = $1")
            .bind(owner_id)
            .execute(pool)
            .await
            .unwrap();
        let (status, _) = get(app, &list, &owner).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "an inactive membership");

        // The administrator sees both blogs on the network list.
        let cookies = admin(app).await;
        let (_, html) = get(
            app,
            &format!("/admin/reports/mail?recipient={}", urlencoding::encode(&to)),
            &cookies,
        )
        .await;
        assert!(html.contains("Ours") && html.contains("Theirs"));
        let (_, html) = get(
            app,
            &format!(
                "/admin/reports/mail?blog={other}&recipient={}",
                urlencoding::encode(&to)
            ),
            &cookies,
        )
        .await;
        assert!(html.contains("Theirs") && !html.contains("Ours"));
    });
}

/// A recording that fails answers 503 and forgets the token, so the
/// provider's retry of the same signed request is recorded, not acknowledged
/// as a replay and dropped.
#[test]
fn a_failed_recording_lets_the_retry_through() {
    run_test(async {
        let app = app();
        let to = address("retried");
        clean_up(&app.db, &to);
        let id = sent_to(app, &to).await;

        // Same key, same Redis, its own pool: closed, so recording fails.
        let broken = TestApp::with_config(|config| {
            config.smtp_host = Some("127.0.0.1".to_string());
            config.smtp_port = 2525;
            config.smtp_encryption = "none".to_string();
            config.smtp_from_email = FROM.to_string();
            config.mail.webhook_signing_key = Some(WebhookKey(KEY.to_string()));
        })
        .await;
        broken.db.close().await;

        let event = complaint(&to, id);
        assert_eq!(
            post_webhook(&broken, &event).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(suppression(&app.db, &to).await, None);

        // Mailgun's retry: the same body and token, to a healthy server.
        assert_eq!(post_webhook(app, &event).await, StatusCode::OK);
        assert_eq!(
            suppression(&app.db, &to).await.as_deref(),
            Some("complaint"),
            "the retry after a 503 was swallowed as a replay"
        );
        let complained: bool =
            sqlx::query_scalar("SELECT complained_at IS NOT NULL FROM mail_outbox WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(complained);
    });
}
