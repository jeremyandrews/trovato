#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The durable mail outbox (BG18, `docs/design/mail-queue.md`).
//!
//! Every message is a `mail_outbox` row written with the write that caused it
//! and sent by a worker. These tests hold the promises the design makes:
//! nothing is lost while the mail server is down, a rolled back write sends
//! nothing, a retried request sends once, a permanent refusal stops at once,
//! account mail never waits behind bulk mail, a bounced address is not mailed
//! again, a blog over its limit is held and not dropped, two workers never send
//! one message twice, and a stopped worker leaves nothing claimed.
//!
//! The messages go over real SMTP to a loopback sink, and each test drains only
//! its own rows (its own blog, or its own recipients), because the outbox is
//! shared by every test using the database. Requires Postgres and Redis.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::smtp_sink::{Envelope, SmtpSink};
use common::{TestApp, defer_cleanup, run_test, shared_app};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use trovato_kernel::config::MailConfig;
use trovato_kernel::models::CreateUser;
use trovato_kernel::services::email::{EmailService, MailSecret, NewMail, SECRET_MARKER};
use trovato_kernel::services::mail_transport::SmtpTransport;
use trovato_kernel::services::mail_worker::{DrainScope, Lane, MailWorker};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const FROM: &str = "noreply@bg18.example.test";

/// An outbox over `pool` whose transport is SMTP on `port`.
fn outbox(pool: &PgPool, port: u16, config: MailConfig) -> Arc<EmailService> {
    let transport = SmtpTransport::new("127.0.0.1", port, None, None, "none").expect("transport");
    Arc::new(EmailService::new(
        pool.clone(),
        Arc::new(transport),
        FROM.to_string(),
        "http://localhost:3000".to_string(),
        config,
    ))
}

/// A blog of its own for one test, removed afterwards with its mail.
async fn blog(pool: &PgPool, limits: Option<serde_json::Value>) -> Uuid {
    let id = Uuid::now_v7();
    let data = limits.map_or_else(
        || serde_json::json!({}),
        |l| serde_json::json!({ "mail_limits": l }),
    );
    sqlx::query("INSERT INTO tenant (id, name, machine_name, data) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(format!("BG18 blog {}", id.simple()))
        .bind(format!("bg18_{}", id.simple()))
        .bind(data)
        .execute(pool)
        .await
        .expect("create a test blog");
    let pool = pool.clone();
    defer_cleanup(move || async move {
        sqlx::query("DELETE FROM tenant WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    });
    id
}

/// A unique address in this test's domain.
fn address(tag: &str) -> String {
    format!("{tag}-{}@bg18.example.test", Uuid::now_v7().simple())
}

fn mail_to(sink: &SmtpSink, address: &str) -> Vec<Envelope> {
    sink.messages()
        .into_iter()
        .filter(|m| m.recipients.iter().any(|r| r == address))
        .collect()
}

/// `(status, attempts, last_error, held_reason)` of one row.
async fn row(pool: &PgPool, id: i64) -> (String, i32, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT status, attempts, last_error, held_reason FROM mail_outbox WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Make a rescheduled row due now: what waiting out its backoff would do.
async fn due_now(pool: &PgPool, id: i64) {
    sqlx::query("UPDATE mail_outbox SET next_attempt_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

/// The outbox row queued for `recipient`, waiting up to five seconds for the
/// request's spawned task to write it.
async fn wait_for_row(pool: &PgPool, recipient: &str) -> Vec<i64> {
    for _ in 0..50 {
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM mail_outbox WHERE recipient = $1 ORDER BY id")
                .bind(recipient)
                .fetch_all(pool)
                .await
                .unwrap();
        if !ids.is_empty() {
            return ids;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Vec::new()
}

async fn reset_limits(app: &TestApp, ip: &str, email: &str) {
    let limiter = app.state.rate_limiter();
    limiter.reset("recovery", ip).await.ok();
    limiter
        .reset("recovery", &format!("password-reset:{ip}"))
        .await
        .ok();
    limiter
        .reset("reset_mail", &email.to_lowercase())
        .await
        .ok();
}

async fn request_reset(app: &TestApp, email: &str, ip: &str) -> StatusCode {
    reset_limits(app, ip, email).await;
    app.request(
        Request::post("/user/password-reset")
            .header("content-type", "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({ "email": email }).to_string(),
            ))
            .unwrap(),
    )
    .await
    .status()
}

/// An app whose mail goes to SMTP on `port`.
async fn app_mailing(port: u16) -> TestApp {
    TestApp::with_config(|config| {
        config.smtp_host = Some("127.0.0.1".to_string());
        config.smtp_port = port;
        config.smtp_encryption = "none".to_string();
        config.smtp_from_email = FROM.to_string();
    })
    .await
}

/// The path of the first `/user/password-reset/<token>` link in a message.
fn reset_path(message: &Envelope) -> String {
    let data = message.data.replace("=\r\n", "").replace("=\n", "");
    let start = data
        .find("/user/password-reset/")
        .expect("the message carries a reset link");
    data[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
        .collect()
}

// ---------------------------------------------------------------------------
// Nothing lost while the mail server is down
// ---------------------------------------------------------------------------

/// With the mail server down, a password reset is queued, not lost; when the
/// server returns it is delivered once, with a working link minted at send
/// time, and the stored row never held the secret.
#[test]
fn a_password_reset_waits_out_a_mail_server_outage() {
    run_test(async {
        let port = SmtpSink::free_port().await;
        let app = app_mailing(port).await;
        let name = common::username("bg18reset");
        let email = format!("{name}@bg18.example.test");
        app.create_test_user(&name, "Bg18-Password-123", &email)
            .await;

        let ip = common::test_ip_for(&name);
        assert_eq!(request_reset(&app, &email, &ip).await, StatusCode::OK);
        let ids = wait_for_row(&app.db, &email).await;
        assert_eq!(ids.len(), 1, "the reset is queued");
        let id = ids[0];

        // The server is down: the attempt fails for now and is rescheduled.
        let stats = app.deliver_mail_to(&[&email]).await;
        assert_eq!(stats.retried, 1, "{stats:?}");
        let (status, attempts, error, _) = row(&app.db, id).await;
        assert_eq!(status, "queued");
        assert_eq!(attempts, 1);
        assert!(
            error.unwrap_or_default().contains("SMTP"),
            "the reason is kept"
        );

        // The server comes back on the same port.
        let sink = SmtpSink::start_on(port).await;
        due_now(&app.db, id).await;
        let stats = app.deliver_mail_to(&[&email]).await;
        assert_eq!(stats.sent, 1, "{stats:?}");
        let delivered = mail_to(&sink, &email);
        assert_eq!(delivered.len(), 1, "delivered exactly once");
        assert_eq!(row(&app.db, id).await.0, "sent");

        // The link works: the token was minted when the message was sent.
        let path = reset_path(&delivered[0]);
        assert!(!path.contains(SECRET_MARKER), "{path}");
        let page = app
            .request(Request::get(&path).body(Body::empty()).unwrap())
            .await;
        assert_eq!(page.status(), StatusCode::OK, "the reset link is valid");

        // And the stored body holds the marker, never the token.
        let stored: String = sqlx::query_scalar("SELECT text_body FROM mail_outbox WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let token = path.trim_start_matches("/user/password-reset/");
        assert!(stored.contains(SECRET_MARKER), "{stored}");
        assert!(!stored.contains(token), "the secret is never stored");

        // Draining again sends nothing more.
        let stats = app.deliver_mail_to(&[&email]).await;
        assert_eq!(stats.claimed(), 0, "{stats:?}");
        assert_eq!(mail_to(&sink, &email).len(), 1);
        sink.stop().await;
    });
}

/// A temporary failure is retried until the attempt cap, then the message is
/// failed with its reason, not dropped.
#[test]
fn temporary_failures_stop_at_the_attempt_cap_and_stay_visible() {
    run_test(async {
        let app = shared_app().await;
        let port = SmtpSink::free_port().await;
        let mail = outbox(
            &app.db,
            port,
            MailConfig {
                max_attempts: 2,
                ..MailConfig::default()
            },
        );
        let tenant = blog(&app.db, None).await;
        let to = address("cap");
        let id = mail
            .queue_now(NewMail::notification("test", &to, "Capped", "Body").tenant(tenant))
            .await
            .unwrap()
            .unwrap();
        let worker = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));

        assert_eq!(worker.drain().await.unwrap().retried, 1);
        due_now(&app.db, id).await;
        let stats = worker.drain().await.unwrap();
        assert_eq!(stats.failed, 1, "{stats:?}");
        let (status, attempts, error, _) = row(&app.db, id).await;
        assert_eq!(status, "failed");
        assert_eq!(attempts, 2);
        assert!(
            error.unwrap_or_default().contains("after 2 attempts"),
            "the reason says why it stopped"
        );
    });
}

// ---------------------------------------------------------------------------
// Same transaction as the event
// ---------------------------------------------------------------------------

/// A message queued in a transaction that rolls back is never sent; one queued
/// in a transaction that commits always is.
#[test]
fn a_rolled_back_transaction_queues_nothing() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let tenant = blog(&app.db, None).await;
        let rolled_back = address("rollback");
        let committed = address("commit");

        let mut tx = app.db.begin().await.unwrap();
        mail.queue(
            &mut *tx,
            NewMail::account("test", &rolled_back, "Never", "Body").tenant(tenant),
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();

        let mut tx = app.db.begin().await.unwrap();
        mail.queue(
            &mut *tx,
            NewMail::account("test", &committed, "Always", "Body").tenant(tenant),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE recipient = $1")
                .bind(&rolled_back)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(queued, 0, "a rolled back write leaves no message");

        let stats = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant))
            .drain()
            .await
            .unwrap();
        assert_eq!(stats.sent, 1, "{stats:?}");
        assert!(mail_to(&sink, &rolled_back).is_empty());
        assert_eq!(mail_to(&sink, &committed).len(), 1);
    });
}

/// Registration writes the account and its verification mail in one
/// transaction: a message that cannot be queued takes the account with it, and
/// one that is queued carries a link, minted when sent, that activates the
/// account. Goes through the user service, as the registration route does, so
/// the shared registration setting is left alone.
#[test]
fn a_registration_queues_its_verification_with_the_account() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let system = UserContext::administrator(Uuid::nil(), vec![]);
        let input = |name: &str, email: &str| CreateUser {
            name: name.to_string(),
            password: "Bg18-Password-123".to_string(),
            mail: email.to_string(),
            is_admin: false,
        };
        let users_named = |name: String| {
            let pool = app.db.clone();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE name = $1")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };

        // A message the outbox refuses (a recipient that is not an address)
        // rolls the account back with it.
        let refused = common::username("bg18atomic");
        let refused_mail = format!("{refused}@bg18.example.test");
        let result = app
            .state
            .users()
            .create_with_status_and_mail(
                input(&refused, &refused_mail),
                0,
                &system,
                &mail,
                |_| {
                    vec![NewMail::account(
                        "registration_verify",
                        "not an address",
                        "Verify",
                        "Body",
                    )]
                },
                "registration",
            )
            .await;
        assert!(result.is_err(), "the refused message fails the write");
        assert_eq!(
            users_named(refused.clone()).await,
            0,
            "the account rolled back with its mail"
        );

        // A good one commits with the account.
        let name = common::username("bg18reg");
        let email = format!("{name}@bg18.example.test");
        let user = app
            .state
            .users()
            .create_with_status_and_mail(
                input(&name, &email),
                0,
                &system,
                &mail,
                |user| {
                    vec![
                        NewMail::account(
                            "registration_verify",
                            &email,
                            "Verify your account",
                            &format!("Visit http://localhost:3000/user/verify/{SECRET_MARKER}"),
                        )
                        .secret(MailSecret::Verification {
                            user_id: user.id,
                            purpose: "registration".to_string(),
                        }),
                    ]
                },
                "registration",
            )
            .await
            .unwrap();
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT category, status FROM mail_outbox WHERE recipient = $1")
                .bind(&email)
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![("registration_verify".to_string(), "queued".to_string())],
            "queued with the account"
        );

        let stats = MailWorker::new(
            &mail,
            Lane::Any,
            DrainScope::Recipients(vec![email.clone()]),
        )
        .drain()
        .await
        .unwrap();
        assert_eq!(stats.sent, 1, "{stats:?}");
        let message = &mail_to(&sink, &email)[0];
        let data = message.data.replace("=\r\n", "").replace("=\n", "");
        let start = data.find("/user/verify/").expect("a verification link");
        let path: String = data[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
            .collect();
        assert!(!path.contains(SECRET_MARKER), "{path}");
        app.request(Request::get(&path).body(Body::empty()).unwrap())
            .await;
        let status: i16 = sqlx::query_scalar("SELECT status FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(status, 1, "the minted link activated the account");
        sink.stop().await;
    });
}

/// A secret is minted only for the address the account has when the message
/// is sent. A reset queued for an address the member has since changed fails
/// for good and mints nothing; nothing reaches the old address.
#[test]
fn a_secret_is_never_minted_for_an_address_the_account_left() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let name = common::username("bg18moved");
        let old = format!("{name}-old@bg18.example.test");
        app.create_test_user(&name, "Bg18-Password-123", &old).await;
        let user_id = common::user_id_of(app, &name).await;
        let id = mail
            .queue_now(
                NewMail::account(
                    "password_reset",
                    &old,
                    "Reset",
                    &format!("http://localhost:3000/user/password-reset/{SECRET_MARKER}"),
                )
                .secret(MailSecret::PasswordReset { user_id }),
            )
            .await
            .unwrap()
            .unwrap();

        // The member moves to a new address before the message goes.
        sqlx::query("UPDATE users SET mail = $2 WHERE id = $1")
            .bind(user_id)
            .bind(format!("{name}-new@bg18.example.test"))
            .execute(&app.db)
            .await
            .unwrap();
        let tokens_before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM password_reset_tokens WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();

        let stats = MailWorker::new(&mail, Lane::Any, DrainScope::Recipients(vec![old.clone()]))
            .drain()
            .await
            .unwrap();
        assert_eq!(stats.failed, 1, "{stats:?}");
        let (status, _, error, _) = row(&app.db, id).await;
        assert_eq!(status, "failed");
        assert!(
            error.unwrap_or_default().contains("address changed"),
            "the reason says why"
        );
        assert!(
            mail_to(&sink, &old).is_empty(),
            "nothing went to the old address"
        );
        let tokens_after: i64 =
            sqlx::query_scalar("SELECT count(*) FROM password_reset_tokens WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(tokens_after, tokens_before, "no token was minted");
        sink.stop().await;
    });
}

/// An email change's link is minted only for the address now pending: a
/// confirmation queued for an address the member has since replaced fails and
/// mints nothing; the one for the current pending address goes, and its stored
/// body never holds the token.
#[test]
fn an_email_change_secret_goes_only_to_the_pending_address() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let name = common::username("bg18change");
        app.create_test_user(
            &name,
            "Bg18-Password-123",
            &format!("{name}@bg18.example.test"),
        )
        .await;
        let user_id = common::user_id_of(app, &name).await;
        let first = format!("{name}-first@bg18.example.test");
        let second = format!("{name}-second@bg18.example.test");
        let mut ids = Vec::new();
        for to in [&first, &second] {
            sqlx::query(
                "UPDATE users SET data = coalesce(data, '{}'::jsonb) \
                 || jsonb_build_object('pending_email', $2::text) WHERE id = $1",
            )
            .bind(user_id)
            .bind(to)
            .execute(&app.db)
            .await
            .unwrap();
            ids.push(
                mail.queue_now(
                    NewMail::account(
                        "email_change_verify",
                        to,
                        "Confirm",
                        &format!("http://localhost:3000/user/verify-email/{SECRET_MARKER}"),
                    )
                    .secret(MailSecret::Verification {
                        user_id,
                        purpose: "email_change".to_string(),
                    }),
                )
                .await
                .unwrap()
                .unwrap(),
            );
        }
        let tokens = || {
            let pool = app.db.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM email_verification_tokens \
                     WHERE user_id = $1 AND purpose = 'email_change'",
                )
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let before = tokens().await;

        let stats = MailWorker::new(
            &mail,
            Lane::Any,
            DrainScope::Recipients(vec![first.clone(), second.clone()]),
        )
        .drain()
        .await
        .unwrap();
        assert_eq!((stats.sent, stats.failed), (1, 1), "{stats:?}");
        let (status, _, error, _) = row(&app.db, ids[0]).await;
        assert_eq!(status, "failed");
        assert!(error.unwrap_or_default().contains("address changed"));
        assert!(
            mail_to(&sink, &first).is_empty(),
            "nothing went to the replaced address"
        );
        assert_eq!(row(&app.db, ids[1]).await.0, "sent");
        assert_eq!(mail_to(&sink, &second).len(), 1);
        assert_eq!(
            tokens().await,
            before + 1,
            "one token, for the pending address only"
        );
        let stored: String = sqlx::query_scalar("SELECT text_body FROM mail_outbox WHERE id = $1")
            .bind(ids[1])
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(stored.contains(SECRET_MARKER), "{stored}");
        sink.stop().await;
    });
}

/// A recovery code is minted when it is sent, bound to its flow, redeemable,
/// and never stored in the outbox.
#[test]
fn a_recovery_code_is_minted_at_send_for_its_flow_and_never_stored() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let name = common::username("bg18recover");
        let email = format!("{name}@bg18.example.test");
        app.create_test_user(&name, "Bg18-Password-123", &email)
            .await;
        let user_id = common::user_id_of(app, &name).await;
        let flow_id = format!("bg18-flow-{}", Uuid::now_v7().simple());
        let id = mail
            .queue_now(
                NewMail::account(
                    "recovery_code",
                    &email,
                    "Code",
                    &format!("Your code: {SECRET_MARKER}\n"),
                )
                .secret(MailSecret::RecoveryCode {
                    user_id,
                    flow_id: flow_id.clone(),
                }),
            )
            .await
            .unwrap()
            .unwrap();
        let none_yet: i64 =
            sqlx::query_scalar("SELECT count(*) FROM recovery_email_challenges WHERE flow_id = $1")
                .bind(&flow_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(none_yet, 0, "nothing is minted while the message waits");

        let stats = MailWorker::new(
            &mail,
            Lane::Any,
            DrainScope::Recipients(vec![email.clone()]),
        )
        .drain()
        .await
        .unwrap();
        assert_eq!(stats.sent, 1, "{stats:?}");
        let data = mail_to(&sink, &email)[0].data.replace("=\r\n", "");
        let code: String = data
            .split("Your code: ")
            .nth(1)
            .expect("the code")
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        assert!(!code.is_empty(), "the marker was replaced: {data}");
        let (owner, hash): (Uuid, String) = sqlx::query_as(
            "SELECT user_id, code_hash FROM recovery_email_challenges \
             WHERE flow_id = $1 AND used_at IS NULL AND expires_at > now()",
        )
        .bind(&flow_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(owner, user_id);
        assert!(
            trovato_kernel::services::recovery_flow::secret_matches(&code, &hash),
            "the mailed code redeems this flow's challenge"
        );
        let stored: String = sqlx::query_scalar("SELECT text_body FROM mail_outbox WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(stored.contains(SECRET_MARKER), "{stored}");
        assert!(!stored.contains(&code), "the code is never stored");

        // Once the challenge is used, a late or repeated send for the same
        // flow fails for good and does not open the challenge again.
        sqlx::query("UPDATE recovery_email_challenges SET used_at = now() WHERE flow_id = $1")
            .bind(&flow_id)
            .execute(&app.db)
            .await
            .unwrap();
        let again = mail
            .queue_now(
                NewMail::account(
                    "recovery_code",
                    &email,
                    "Code",
                    &format!("Your code: {SECRET_MARKER}\n"),
                )
                .secret(MailSecret::RecoveryCode {
                    user_id,
                    flow_id: flow_id.clone(),
                }),
            )
            .await
            .unwrap()
            .unwrap();
        let stats = MailWorker::new(
            &mail,
            Lane::Any,
            DrainScope::Recipients(vec![email.clone()]),
        )
        .drain()
        .await
        .unwrap();
        assert_eq!(stats.failed, 1, "{stats:?}");
        assert_eq!(row(&app.db, again).await.0, "failed");
        let (still_used, same_hash): (bool, bool) = sqlx::query_as(
            "SELECT used_at IS NOT NULL, code_hash = $2 FROM recovery_email_challenges \
             WHERE flow_id = $1",
        )
        .bind(&flow_id)
        .bind(&hash)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(still_used && same_hash, "the used challenge stays used");
        assert_eq!(mail_to(&sink, &email).len(), 1, "no second code was sent");
        sink.stop().await;
    });
}

/// An authentication refusal is 5xx, but it means the site's credentials are
/// wrong, not the message: it is retried like an outage, never failed at once.
#[test]
fn an_authentication_refusal_is_retried_not_failed() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let tenant = blog(&app.db, None).await;
        let to = address("authfail");
        let id = mail
            .queue_now(NewMail::account("test", &to, "Auth", "Body").tenant(tenant))
            .await
            .unwrap()
            .unwrap();
        let stats = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant))
            .drain()
            .await
            .unwrap();
        assert_eq!((stats.retried, stats.failed), (1, 0), "{stats:?}");
        let (status, attempts, error, _) = row(&app.db, id).await;
        assert_eq!(status, "queued");
        assert_eq!(attempts, 1);
        assert!(
            error.unwrap_or_default().contains("535"),
            "the reason is kept"
        );
        let later: bool =
            sqlx::query_scalar("SELECT next_attempt_at > now() FROM mail_outbox WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(later, "the retry waits out a backoff");
        sink.stop().await;
    });
}

// ---------------------------------------------------------------------------
// Once, and only once
// ---------------------------------------------------------------------------

/// A password reset asked for twice (a double submit, a retried request) is
/// queued and sent once: the dedupe key makes the second a no-op.
#[test]
fn a_retried_request_with_the_same_dedupe_key_sends_once() {
    run_test(async {
        let sink = SmtpSink::start().await;
        let app = app_mailing(sink.port).await;
        let name = common::username("bg18twice");
        let email = format!("{name}@bg18.example.test");
        app.create_test_user(&name, "Bg18-Password-123", &email)
            .await;

        // The reset's dedupe key carries a ten-minute window: both requests
        // must fall in the same one.
        let into = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            % 600;
        if into > 585 {
            tokio::time::sleep(std::time::Duration::from_secs(600 - into + 1)).await;
        }
        let first = common::test_ip_for(&format!("{name}-1"));
        let second = common::test_ip_for(&format!("{name}-2"));
        assert_eq!(request_reset(&app, &email, &first).await, StatusCode::OK);
        assert_eq!(wait_for_row(&app.db, &email).await.len(), 1);
        assert_eq!(request_reset(&app, &email, &second).await, StatusCode::OK);
        // The second request's spawned task has had time to try.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert_eq!(
            wait_for_row(&app.db, &email).await.len(),
            1,
            "the second request queued nothing"
        );

        assert_eq!(app.deliver_mail_to(&[&email]).await.sent, 1);
        assert_eq!(mail_to(&sink, &email).len(), 1, "one message arrived");
        sink.stop().await;
    });
}

/// Two workers draining the same blog at once never send one message twice.
#[test]
fn two_workers_never_send_the_same_message() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(
            &app.db,
            sink.port,
            MailConfig {
                batch: 3,
                ..MailConfig::default()
            },
        );
        let tenant = blog(&app.db, None).await;
        let recipients: Vec<String> = (0..40).map(|_| address("pair")).collect();
        for to in &recipients {
            mail.queue_now(NewMail::notification("test", to, "Pair", "Body").tenant(tenant))
                .await
                .unwrap();
        }

        let one = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));
        let two = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));
        let (a, b) = tokio::join!(one.drain(), two.drain());
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a.sent + b.sent, 40, "{a:?} {b:?}");
        assert!(a.sent > 0 && b.sent > 0, "both took part: {a:?} {b:?}");
        for to in &recipients {
            assert_eq!(mail_to(&sink, to).len(), 1, "{to} got exactly one");
        }
    });
}

// ---------------------------------------------------------------------------
// Permanent failures, bounces and suppression
// ---------------------------------------------------------------------------

/// A permanent refusal (SMTP 5xx for the recipient) fails the message at once,
/// with the server's answer as the reason, and it is never tried again.
#[test]
fn a_permanent_failure_stops_retrying() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let tenant = blog(&app.db, None).await;
        let to = address("bounce");
        let id = mail
            .queue_now(NewMail::notification("test", &to, "Refused", "Body").tenant(tenant))
            .await
            .unwrap()
            .unwrap();
        let worker = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));

        assert_eq!(worker.drain().await.unwrap().failed, 1);
        let (status, attempts, error, _) = row(&app.db, id).await;
        assert_eq!(status, "failed");
        assert_eq!(attempts, 1, "no retry after a permanent refusal");
        assert!(
            error.unwrap_or_default().contains("550"),
            "the server's answer is kept"
        );

        due_now(&app.db, id).await;
        assert_eq!(
            worker.drain().await.unwrap().claimed(),
            0,
            "never claimed again"
        );
        assert!(mail_to(&sink, &to).is_empty());
    });
}

/// A hard bounce suppresses every later message to the address, for every
/// blog; a complaint suppresses bulk mail only, so account mail still goes.
#[test]
fn a_bounced_or_complaining_address_is_suppressed() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let one = blog(&app.db, None).await;
        let two = blog(&app.db, None).await;
        let bounced = address("hard");
        let complained = address("spam");
        for (addr, reason) in [(&bounced, "hard_bounce"), (&complained, "complaint")] {
            sqlx::query("INSERT INTO mail_suppression (address, reason) VALUES ($1, $2)")
                .bind(addr.to_lowercase())
                .bind(reason)
                .execute(&app.db)
                .await
                .unwrap();
            let pool = app.db.clone();
            let addr = addr.to_lowercase();
            defer_cleanup(move || async move {
                sqlx::query("DELETE FROM mail_suppression WHERE address = $1")
                    .bind(addr)
                    .execute(&pool)
                    .await
                    .ok();
            });
        }

        // The bounce was recorded once; both blogs are stopped by it.
        for tenant in [one, two] {
            mail.queue_now(NewMail::account("test", &bounced, "Account", "Body").tenant(tenant))
                .await
                .unwrap();
        }
        mail.queue_now(
            NewMail::bulk("test", &complained, "News", "Body", "https://x.test/u/1").tenant(one),
        )
        .await
        .unwrap();
        mail.queue_now(NewMail::account("test", &complained, "Reset", "Body").tenant(one))
            .await
            .unwrap();

        let mut suppressed = 0;
        for tenant in [one, two] {
            let stats = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant))
                .drain()
                .await
                .unwrap();
            suppressed += stats.suppressed;
        }
        assert_eq!(
            suppressed, 3,
            "two to the bounced address, one bulk to the complainer"
        );
        assert!(
            mail_to(&sink, &bounced).is_empty(),
            "nothing reaches a bounced address"
        );
        let to_complainer = mail_to(&sink, &complained);
        assert_eq!(
            to_complainer.len(),
            1,
            "account mail still reaches a complainer"
        );
        assert!(to_complainer[0].data.contains("Subject: Reset"));

        let reasons: Vec<String> = sqlx::query_scalar(
            "SELECT last_error FROM mail_outbox WHERE recipient = $1 AND status = 'suppressed'",
        )
        .bind(&bounced)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(reasons.len(), 2);
        assert!(reasons.iter().all(|r| r.contains("bounced")), "{reasons:?}");
    });
}

// ---------------------------------------------------------------------------
// Priority and limits
// ---------------------------------------------------------------------------

/// A verification mail queued behind a newsletter is sent first, and the
/// transactional lane never takes bulk mail at all.
#[test]
fn bulk_mail_never_delays_a_queued_verification() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let tenant = blog(&app.db, None).await;
        let readers: Vec<String> = (0..25).map(|_| address("reader")).collect();
        for to in &readers {
            mail.queue_now(
                NewMail::bulk("newsletter", to, "Newsletter", "Body", "https://x.test/u/1")
                    .tenant(tenant),
            )
            .await
            .unwrap();
        }
        let member = address("member");
        mail.queue_now(
            NewMail::account("registration_verify", &member, "Verify", "Body").tenant(tenant),
        )
        .await
        .unwrap();

        // The transactional lane sends the verification and leaves the bulk.
        let lane = MailWorker::new(&mail, Lane::Transactional, DrainScope::Tenant(tenant));
        let stats = lane.drain().await.unwrap();
        assert_eq!(stats.sent, 1, "{stats:?}");
        assert_eq!(mail_to(&sink, &member).len(), 1);
        assert!(readers.iter().all(|r| mail_to(&sink, r).is_empty()));

        // On the general lane, a verification queued after the newsletter is
        // still the next message sent.
        let late = address("late");
        mail.queue_now(
            NewMail::account("registration_verify", &late, "Verify", "Body").tenant(tenant),
        )
        .await
        .unwrap();
        let general = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));
        general.run_once(None).await.unwrap();
        let order: Vec<String> = sink
            .messages()
            .into_iter()
            .flat_map(|m| m.recipients)
            .filter(|r| r == &late || readers.contains(r))
            .collect();
        assert_eq!(
            order.first(),
            Some(&late),
            "the verification went first: {order:?}"
        );
        general.drain().await.unwrap();
        assert!(readers.iter().all(|r| mail_to(&sink, r).len() == 1));
    });
}

/// A blog over its hourly limit is held, not dropped: the rest wait with the
/// reason on the row, and go when the limit allows.
#[test]
fn a_blog_over_its_limit_is_held_not_dropped() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        // A batch of two: the last message is never claimed, so it is held
        // only through the blog-wide hold, not by being put back.
        let mail = outbox(
            &app.db,
            sink.port,
            MailConfig {
                batch: 2,
                ..MailConfig::default()
            },
        );
        let tenant = blog(&app.db, Some(serde_json::json!({ "hourly": 2 }))).await;
        let readers: Vec<String> = (0..5).map(|_| address("capped")).collect();
        for to in &readers {
            mail.queue_now(
                NewMail::bulk("newsletter", to, "Capped", "Body", "https://x.test/u/1")
                    .tenant(tenant),
            )
            .await
            .unwrap();
        }

        let stats = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant))
            .drain()
            .await
            .unwrap();
        assert_eq!(stats.sent, 2, "{stats:?}");
        let held: Vec<(String, i32, Option<String>)> = sqlx::query_as(
            "SELECT status, attempts, held_reason FROM mail_outbox
             WHERE tenant_id = $1 AND status <> 'sent'",
        )
        .bind(tenant)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(held.len(), 3, "{held:?}");
        for (status, attempts, reason) in &held {
            assert_eq!(status, "queued", "held, not failed or dropped");
            assert_eq!(*attempts, 0, "holding does not spend an attempt");
            assert!(
                reason.as_deref().unwrap_or_default().contains("hourly"),
                "the reason is on the row: {reason:?}"
            );
        }

        // Held rows wait out the hold in the table, not just in this worker.
        let waiting: bool = sqlx::query_scalar(
            "SELECT bool_and(next_attempt_at > now()) FROM mail_outbox
             WHERE tenant_id = $1 AND held_reason IS NOT NULL",
        )
        .bind(tenant)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(waiting, "held rows are not due again until the hold ends");
        sqlx::query("UPDATE mail_outbox SET next_attempt_at = now() WHERE tenant_id = $1")
            .bind(tenant)
            .execute(&app.db)
            .await
            .unwrap();

        // The network raises the blog's limit; the held mail goes.
        sqlx::query("UPDATE tenant SET data = $2 WHERE id = $1")
            .bind(tenant)
            .bind(serde_json::json!({ "mail_limits": { "hourly": 10 } }))
            .execute(&app.db)
            .await
            .unwrap();
        let stats = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant))
            .drain()
            .await
            .unwrap();
        assert_eq!(stats.sent, 3, "{stats:?}");
        assert!(readers.iter().all(|r| mail_to(&sink, r).len() == 1));
    });
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// Retention clears the bodies of finished mail after a week and deletes
/// finished rows after 90 days; unfinished mail is never touched.
#[test]
fn retention_clears_old_bodies_and_deletes_old_rows() {
    run_test(async {
        let app = shared_app().await;
        let mail = outbox(&app.db, SmtpSink::free_port().await, MailConfig::default());
        let tenant = blog(&app.db, None).await;
        let mut ids = Vec::new();
        for tag in ["fresh", "old", "ancient", "waiting"] {
            ids.push(
                mail.queue_now(
                    NewMail::notification("test", &address(tag), "Kept", "Body").tenant(tenant),
                )
                .await
                .unwrap()
                .unwrap(),
            );
        }
        for (id, days) in [(ids[0], 0_i32), (ids[1], 8), (ids[2], 91)] {
            sqlx::query(
                "UPDATE mail_outbox SET status = 'sent',
                        sent_at = now() - make_interval(days => $2),
                        finished_at = now() - make_interval(days => $2)
                 WHERE id = $1",
            )
            .bind(id)
            .bind(days)
            .execute(&app.db)
            .await
            .unwrap();
        }
        // Still queued, created long ago, and not due (so no drain takes it).
        sqlx::query(
            "UPDATE mail_outbox SET created_at = now() - interval '91 days',
                    next_attempt_at = now() + interval '1 day' WHERE id = $1",
        )
        .bind(ids[3])
        .execute(&app.db)
        .await
        .unwrap();

        // The batch is large: the purge is network wide, and old rows from
        // other runs must not use up its limit before this test's are reached.
        trovato_kernel::services::mail_worker::purge(&app.db, &MailConfig::default(), 1_000_000)
            .await
            .unwrap();

        let rows: Vec<(i64, bool, bool)> = sqlx::query_as(
            "SELECT id, text_body IS NOT NULL, body_purged_at IS NOT NULL
             FROM mail_outbox WHERE tenant_id = $1 ORDER BY id",
        )
        .bind(tenant)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (ids[0], true, false),
                (ids[1], false, true),
                (ids[3], true, false)
            ],
            "fresh kept, week-old body cleared, 90-day row deleted, queued untouched"
        );
    });
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

/// A worker told to stop puts back what it claimed and did not send, without
/// spending an attempt; a worker that died is replaced once its lease expires.
#[test]
fn worker_shutdown_leaves_no_message_claimed_forever() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let tenant = blog(&app.db, None).await;
        for _ in 0..5 {
            mail.queue_now(
                NewMail::notification("test", &address("stop"), "Stop", "Body").tenant(tenant),
            )
            .await
            .unwrap();
        }

        let stop = CancellationToken::new();
        stop.cancel();
        let worker = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));
        let stats = worker.run_once(Some(&stop)).await.unwrap();
        assert_eq!(stats.released, 5, "{stats:?}");
        let left: Vec<(String, i32, Option<String>)> = sqlx::query_as(
            "SELECT status, attempts, locked_by FROM mail_outbox WHERE tenant_id = $1",
        )
        .bind(tenant)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert!(
            left.iter()
                .all(|(s, a, l)| s == "queued" && *a == 0 && l.is_none()),
            "{left:?}"
        );

        // A worker that died holding a claim: its lease has run out.
        sqlx::query(
            "UPDATE mail_outbox SET status = 'sending', attempts = 1, locked_by = 'dead',
                    locked_until = now() - interval '1 second'
             WHERE tenant_id = $1",
        )
        .bind(tenant)
        .execute(&app.db)
        .await
        .unwrap();
        let stats = worker.drain().await.unwrap();
        assert_eq!(
            stats.sent, 5,
            "the expired claims were taken over: {stats:?}"
        );

        // The run loop stops when told, and leaves nothing claimed.
        mail.queue_now(
            NewMail::notification("test", &address("loop"), "Loop", "Body").tenant(tenant),
        )
        .await
        .unwrap();
        let token = CancellationToken::new();
        let looping = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));
        let handle = tokio::spawn(looping.run(token.clone(), mail.wake_handle()));
        mail.wake();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("the worker stops when told")
            .unwrap();
        let claimed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM mail_outbox WHERE tenant_id = $1 AND status = 'sending'",
        )
        .bind(tenant)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(claimed, 0, "nothing is left claimed");
    });
}

// ---------------------------------------------------------------------------
// Throughput (run by hand)
// ---------------------------------------------------------------------------

/// Send throughput of one worker against the loopback sink, and what queueing
/// costs a caller. A measurement, not a check, so it is ignored by default:
///
/// ```sh
/// cargo test -p trovato-kernel --test mail_queue_test -- --ignored --nocapture throughput
/// ```
#[test]
#[ignore = "a measurement, run by hand"]
fn throughput() {
    run_test(async {
        let app = shared_app().await;
        let sink = SmtpSink::start().await;
        let mail = outbox(&app.db, sink.port, MailConfig::default());
        let tenant = blog(&app.db, None).await;
        let count = 2000;

        let started = std::time::Instant::now();
        for _ in 0..count {
            mail.queue_now(
                NewMail::notification("throughput", &address("tp"), "Throughput", "Body")
                    .tenant(tenant),
            )
            .await
            .unwrap();
        }
        let queued = started.elapsed();

        let worker = MailWorker::new(&mail, Lane::Any, DrainScope::Tenant(tenant));
        let started = std::time::Instant::now();
        let stats = worker.drain().await.unwrap();
        let sent = started.elapsed();
        assert_eq!(stats.sent, count);

        println!(
            "queue: {count} messages in {queued:?}, {:.3} ms each",
            queued.as_secs_f64() * 1000.0 / count as f64
        );
        println!(
            "send: {count} messages in {sent:?} by one worker, {:.0} messages a second",
            count as f64 / sent.as_secs_f64()
        );
    });
}
