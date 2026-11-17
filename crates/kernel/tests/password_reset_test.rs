#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `POST /user/password-reset`: asking for a reset mail.
//!
//! The request must not tell a caller whether an address has an account, and
//! must not let anyone send an unbounded number of reset mails: not from one
//! address, and not to one victim from many addresses. Over either limit the
//! answer is the same success body, so the limit itself reveals nothing.
//!
//! A sent mail is observed as a reset token row, which the handler creates for
//! every mail it sends (the test app has no SMTP configured, so the mail itself
//! is only logged).
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use uuid::Uuid;

/// A distinct address per test, the way `recovery_flow_test` does it, so no
/// two tests share a rate-limit bucket.
fn ip_for(tag: &str) -> String {
    common::test_ip_for(&format!("password-reset-{tag}"))
}

/// A user with a unique name and address; returns `(user_id, email)`.
async fn known_account(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let email = format!("{name}@example.com");
    app.create_test_user(&name, "test-password-123", &email)
        .await;
    (common::user_id_of(app, &name).await, email)
}

/// Clear every limit a reset request from `ip` for `email` could touch.
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

/// Ask for a reset of `email` from `ip`, returning the status and raw body.
async fn request_reset(app: &TestApp, email: &str, ip: &str) -> (StatusCode, Vec<u8>) {
    let response = app
        .request(
            Request::post("/user/password-reset")
                .header("content-type", "application/json")
                .header("x-forwarded-for", ip)
                .body(Body::from(
                    serde_json::json!({ "email": email }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap()
        .to_vec();
    (status, body)
}

/// Reset tokens issued to these users so far.
async fn tokens_for(app: &TestApp, users: &[Uuid]) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM password_reset_tokens WHERE user_id = ANY($1)")
        .bind(users)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// **Guard: a known and an unknown address get the same answer.**
///
/// Already true before the per-address and per-IP limits; kept so they cannot
/// change it.
#[test]
fn a_known_and_an_unknown_address_get_byte_identical_answers() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (_user, email) = known_account(app, "reset-known").await;
        let unknown = format!("nobody-{}@example.com", Uuid::now_v7().simple());
        let ip_a = ip_for(&format!("{email}-a"));
        let ip_b = ip_for(&format!("{email}-b"));
        reset_limits(app, &ip_a, &email).await;
        reset_limits(app, &ip_b, &unknown).await;

        let known_answer = request_reset(app, &email, &ip_a).await;
        let unknown_answer = request_reset(app, &unknown, &ip_b).await;
        assert_eq!(known_answer.0, StatusCode::OK);
        assert_eq!(
            known_answer, unknown_answer,
            "status and body must not distinguish a real account"
        );
    });
}

/// **One address cannot send unlimited reset mails.**
///
/// Eight requests from one address, each for a different real account: past
/// the per-address limit no further mail goes out, and the answer is the same.
#[test]
fn requests_over_the_per_ip_limit_send_no_mail_and_answer_the_same() {
    common::run_test(async {
        let app = common::shared_app().await;
        let ip = ip_for(&Uuid::now_v7().simple().to_string());
        let mut users = Vec::new();
        let mut first_body = None;

        for n in 0..8 {
            let (user, email) = known_account(app, &format!("reset-ip{n}")).await;
            if n == 0 {
                reset_limits(app, &ip, &email).await;
            } else {
                app.state
                    .rate_limiter()
                    .reset("reset_mail", &email.to_lowercase())
                    .await
                    .ok();
            }
            users.push(user);
            let (status, body) = request_reset(app, &email, &ip).await;
            assert_eq!(status, StatusCode::OK, "request {n}");
            match &first_body {
                None => first_body = Some(body),
                Some(first) => assert_eq!(&body, first, "request {n} answers the same"),
            }
        }

        let sent = tokens_for(app, &users).await;
        assert!(
            sent <= 5,
            "at most five reset mails from one address per window, sent {sent}"
        );
    });
}

/// **One victim cannot be flooded from many addresses.**
///
/// Six requests for one account, each from a different address: past the
/// per-account cap no further mail goes out, and the answer is the same.
#[test]
fn requests_over_the_per_address_cap_send_no_mail_and_answer_the_same() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user, email) = known_account(app, "reset-victim").await;
        let mut first_body = None;

        for n in 0..6 {
            let ip = ip_for(&format!("{email}-{n}"));
            if n == 0 {
                reset_limits(app, &ip, &email).await;
            } else {
                // Only the per-IP limits are cleared from here on; the
                // per-account cap is what is under test.
                let limiter = app.state.rate_limiter();
                limiter.reset("recovery", &ip).await.ok();
                limiter
                    .reset("recovery", &format!("password-reset:{ip}"))
                    .await
                    .ok();
            }
            let (status, body) = request_reset(app, &email.to_uppercase(), &ip).await;
            assert_eq!(status, StatusCode::OK, "request {n}");
            match &first_body {
                None => first_body = Some(body),
                Some(first) => assert_eq!(&body, first, "request {n} answers the same"),
            }
        }

        let sent = tokens_for(app, &[user]).await;
        assert!(
            sent <= 3,
            "at most three reset mails to one account per hour, sent {sent}"
        );
    });
}
