#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The reset by email flow as pages a browser can complete (**BL-07**).
//!
//! `/user/password-reset` was POST only and took JSON, and the link a reset
//! mail carried answered JSON, so nobody with a browser could finish a reset.
//! These tests walk it as a browser does: load the request form, post it, read
//! the mail the loopback sink received, follow its link, set a password, land
//! on the login page.
//!
//! They also pin two defects found on the way: the reset mail was built by an
//! untemplated sender beside the `password_reset` email template, and the token
//! was marked used only after the password changed, so two concurrent posts of
//! one token both succeeded.
//!
//! Requires Postgres + Redis.

mod common;

use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use common::smtp_sink::{Envelope, SmtpSink};
use trovato_kernel::models::password_reset::PasswordResetToken;
use uuid::Uuid;

const SITE_FROM: &str = "no-reply@bl07.example.test";
const OLD_PASSWORD: &str = "bl07-Old-Password-123";
const NEW_PASSWORD: &str = "bl07-New-Password-456";

/// The one sentence every request gets, whatever was asked for.
const GENERIC: &str = "If an account with that email exists, a reset link has been sent.";

struct Fixture {
    sink: SmtpSink,
    app: TestApp,
}

static FIXTURE: OnceLock<Fixture> = OnceLock::new();

/// An app whose mail reaches a loopback sink.
fn fixture() -> &'static Fixture {
    FIXTURE.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || {
            handle.block_on(async {
                let sink = SmtpSink::start().await;
                let port = sink.port;
                let app = TestApp::with_config(move |config| {
                    config.smtp_host = Some("127.0.0.1".to_string());
                    config.smtp_port = port;
                    config.smtp_encryption = "none".to_string();
                    config.smtp_from_email = SITE_FROM.to_string();
                })
                .await;
                Fixture { sink, app }
            })
        })
        .join()
        .expect("fixture init thread panicked")
    })
}

fn ip_for(tag: &str) -> String {
    common::test_ip_for(&format!("bl07-{tag}"))
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn csrf_token(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html.find(needle).expect("a _token input") + needle.len();
    let end = start + html[start..].find('"').expect("token end");
    html[start..end].to_string()
}

fn merge_cookies(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}; {b}"),
    }
}

/// A user with a unique name and address; returns `(user_id, email)`.
async fn known_account(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let email = format!("{name}@bl07.example.test");
    app.create_test_user(&name, OLD_PASSWORD, &email).await;
    (common::user_id_of(app, &name).await, email)
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

/// GET a page from `ip` with `cookies`; returns status, new cookies, body.
async fn get(app: &TestApp, path: &str, ip: &str, cookies: &str) -> (StatusCode, String, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let set = common::extract_cookies(&response);
    (
        status,
        merge_cookies(cookies, &set),
        body_text(response).await,
    )
}

/// POST a urlencoded form; returns the response.
async fn post_form(
    app: &TestApp,
    path: &str,
    ip: &str,
    cookies: &str,
    fields: &[(&str, &str)],
) -> axum::response::Response {
    let form = fields
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    app.request_with_cookies(
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("x-forwarded-for", ip)
            .body(Body::from(form))
            .unwrap(),
        cookies,
    )
    .await
}

/// Load the request form and post `email` through it, as a browser does.
async fn request_reset_page(app: &TestApp, email: &str, ip: &str) -> (StatusCode, String) {
    let (status, cookies, html) = get(app, "/user/password-reset", ip, "").await;
    assert_eq!(status, StatusCode::OK, "GET /user/password-reset");
    let token = csrf_token(&html);
    let response = post_form(
        app,
        "/user/password-reset",
        ip,
        &cookies,
        &[("_token", &token), ("email", email)],
    )
    .await;
    let status = response.status();
    (status, body_text(response).await)
}

/// Messages the sink received for `address`.
fn mail_to(sink: &SmtpSink, address: &str) -> Vec<Envelope> {
    sink.messages()
        .into_iter()
        .filter(|m| m.recipients.iter().any(|r| r == address))
        .collect()
}

/// Wait for the spawned send to reach the sink, up to five seconds.
async fn wait_for_mail(sink: &SmtpSink, address: &str, count: usize) -> Vec<Envelope> {
    for _ in 0..50 {
        let found = mail_to(sink, address);
        if found.len() >= count {
            return found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    mail_to(sink, address)
}

/// The reset link's path out of a delivered message.
///
/// The text part may be quoted-printable, which can fold a long line with a
/// trailing `=`; unfold before searching.
fn reset_path(message: &Envelope) -> String {
    let unfolded = message.data.replace("=\r\n", "").replace("=\n", "");
    let start = unfolded
        .find("/user/password-reset/")
        .expect("the mail carries a reset link");
    unfolded[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '/' || *c == '-' || *c == '_')
        .collect()
}

/// Mint a valid token for `user` directly; returns the link path.
async fn minted_link(app: &TestApp, user: Uuid) -> (String, String) {
    let (_, token) = PasswordResetToken::create(&app.db, user).await.unwrap();
    (format!("/user/password-reset/{token}"), token)
}

/// GET the set-password page and POST it with `password` and `confirm`.
async fn set_password(
    app: &TestApp,
    link: &str,
    ip: &str,
    password: &str,
    confirm: &str,
) -> axum::response::Response {
    let (status, cookies, html) = get(app, link, ip, "").await;
    assert_eq!(status, StatusCode::OK, "GET {link}");
    let token = csrf_token(&html);
    post_form(
        app,
        link,
        ip,
        &cookies,
        &[
            ("_token", &token),
            ("password", password),
            ("confirm_password", confirm),
        ],
    )
    .await
}

async fn password_hash(app: &TestApp, user: Uuid) -> String {
    sqlx::query_scalar("SELECT pass FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// The request page
// ---------------------------------------------------------------------------

/// GET `/user/password-reset` is a page with an email form.
#[test]
fn the_request_page_renders_an_email_form() {
    common::run_test(async {
        let app = &fixture().app;
        let (status, _, html) = get(app, "/user/password-reset", &ip_for("form"), "").await;

        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("<form"), "{html}");
        assert!(html.contains(r#"method="post""#), "{html}");
        assert!(html.contains(r#"name="email""#), "an email input: {html}");
        assert!(html.contains(r#"name="_token""#), "a CSRF token: {html}");
    });
}

/// A known address, an unknown one and one over the limit all get the same
/// page, and only the known address is mailed, with the `password_reset`
/// template.
#[test]
fn every_request_gets_the_same_page_and_only_a_known_address_is_mailed() {
    common::run_test(async {
        let fixture = fixture();
        let app = &fixture.app;
        let (_user, known) = known_account(app, "bl07-known").await;
        let unknown = format!("nobody-{}@bl07.example.test", Uuid::now_v7().simple());
        let ip_known = ip_for(&format!("{known}-k"));
        let ip_unknown = ip_for(&format!("{known}-u"));
        reset_limits(app, &ip_known, &known).await;
        reset_limits(app, &ip_unknown, &unknown).await;

        let (known_status, known_page) = request_reset_page(app, &known, &ip_known).await;
        let (unknown_status, unknown_page) = request_reset_page(app, &unknown, &ip_unknown).await;

        assert_eq!(known_status, StatusCode::OK);
        assert_eq!(unknown_status, StatusCode::OK);
        assert!(known_page.contains(GENERIC), "{known_page}");
        assert_eq!(
            known_page, unknown_page,
            "the page must not say whether the address has an account"
        );
        assert!(
            !known_page.contains(&known),
            "the page must not echo the address"
        );

        let mails = wait_for_mail(&fixture.sink, &known, 1).await;
        assert_eq!(mails.len(), 1, "the known address gets one mail");
        let data = &mails[0].data;
        assert!(
            data.contains("multipart/alternative"),
            "sent with the password_reset template (text and HTML): {data}"
        );
        assert!(data.contains("/user/password-reset/"), "{data}");
        assert!(
            mail_to(&fixture.sink, &unknown).is_empty(),
            "an unknown address gets nothing"
        );

        // Over the per-address limit: the same page, and no further mail.
        let before = mail_to(&fixture.sink, &known).len();
        let mut over_page = String::new();
        for n in 0..4 {
            let ip = ip_for(&format!("{known}-over{n}"));
            reset_limits(app, &ip, "unused@bl07.example.test").await;
            over_page = request_reset_page(app, &known, &ip).await.1;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert_eq!(over_page, known_page, "over the limit, the same page");
        assert!(
            mail_to(&fixture.sink, &known).len() <= before + 2,
            "the per-address limit (3 an hour) holds on the page path"
        );
        reset_limits(app, &ip_known, &known).await;
    });
}

// ---------------------------------------------------------------------------
// The link
// ---------------------------------------------------------------------------

/// The mailed link opens a set-password form; it carries `no-referrer` because
/// the token is in the URL.
#[test]
fn the_mailed_link_opens_a_set_password_form() {
    common::run_test(async {
        let fixture = fixture();
        let app = &fixture.app;
        let (_user, email) = known_account(app, "bl07-link").await;
        let ip = ip_for(&format!("{email}-link"));
        reset_limits(app, &ip, &email).await;
        request_reset_page(app, &email, &ip).await;
        let mails = wait_for_mail(&fixture.sink, &email, 1).await;
        let link = reset_path(mails.last().expect("a reset mail"));

        let response = app
            .request(
                Request::get(&link)
                    .header("x-forwarded-for", &ip)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("referrer-policy")
                .and_then(|v| v.to_str().ok()),
            Some("no-referrer"),
            "the token is in the URL, so no referrer may carry it off-site"
        );
        let html = body_text(response).await;
        assert!(html.contains(r#"name="password""#), "{html}");
        assert!(html.contains(r#"name="confirm_password""#), "{html}");
        assert!(html.contains(r#"name="_token""#), "{html}");
    });
}

/// An unknown or an expired link renders a page saying so, with no form.
#[test]
fn an_invalid_or_expired_link_says_so() {
    common::run_test(async {
        let app = &fixture().app;
        let (user, _) = known_account(app, "bl07-expired").await;
        let (expired_link, expired_token) = minted_link(app, user).await;
        sqlx::query(
            "UPDATE password_reset_tokens SET expires_at = now() - interval '1 minute' \
             WHERE token_hash = encode(sha256($1::bytea), 'hex')",
        )
        .bind(expired_token.as_bytes())
        .execute(&app.db)
        .await
        .unwrap();
        let unknown_link = format!("/user/password-reset/{}", "0".repeat(64));

        for link in [&expired_link, &unknown_link] {
            let (_, _, html) = get(app, link, &ip_for("invalid"), "").await;
            assert!(
                html.contains("invalid or has expired"),
                "{link} must say the link is invalid: {html}"
            );
            assert!(
                !html.contains(r#"name="password""#),
                "no form on an invalid link: {html}"
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Setting the password
// ---------------------------------------------------------------------------

/// Matching, strong passwords: the password changes, every session ends, the
/// token is used, the user's other tokens are invalidated, and the browser
/// lands on the login page with a success message.
#[test]
fn a_valid_post_sets_the_password_and_lands_on_the_login_page() {
    common::run_test(async {
        let app = &fixture().app;
        let (user, _) = known_account(app, "bl07-set").await;
        let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let session = app.login(&name, OLD_PASSWORD).await;
        // A session enters the revocation index on its first request after
        // the login, as `session_management_test` notes; make that request,
        // and see the session alive before the reset.
        let alive = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &session,
            )
            .await;
        assert_eq!(alive.status(), StatusCode::OK, "signed in before the reset");
        let (link, token) = minted_link(app, user).await;
        let (_, other_token) = minted_link(app, user).await;
        let before = password_hash(app, user).await;
        let ip = ip_for(&format!("{name}-set"));

        let response = set_password(app, &link, &ip, NEW_PASSWORD, NEW_PASSWORD).await;

        assert!(
            response.status().is_redirection(),
            "a successful reset redirects: {}",
            response.status()
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(location, "/user/login");
        let cookies = common::extract_cookies(&response);

        assert_ne!(
            password_hash(app, user).await,
            before,
            "the password changed"
        );
        assert!(
            PasswordResetToken::find_valid(&app.db, &token)
                .await
                .unwrap()
                .is_none(),
            "the token is used"
        );
        assert!(
            PasswordResetToken::find_valid(&app.db, &other_token)
                .await
                .unwrap()
                .is_none(),
            "the user's other tokens are invalidated"
        );

        // The session that was signed in before is over.
        let profile = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &session,
            )
            .await;
        assert_eq!(
            profile.status(),
            StatusCode::UNAUTHORIZED,
            "every session ends"
        );

        // The login page the browser lands on says it worked.
        let (status, _, login) = get(app, "/user/login", &ip, &cookies).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            login.contains("Your password has been reset"),
            "the login page shows the success message: {login}"
        );

        // And the new password works.
        app.login(&name, NEW_PASSWORD).await;
    });
}

/// A mismatch or a weak password re-renders the form with the error and leaves
/// the token valid.
#[test]
fn a_mismatch_or_a_weak_password_rerenders_and_keeps_the_token() {
    common::run_test(async {
        let app = &fixture().app;
        let (user, _) = known_account(app, "bl07-mismatch").await;
        let (link, token) = minted_link(app, user).await;
        let before = password_hash(app, user).await;
        let ip = ip_for(&format!("{user}-mismatch"));

        let mismatch = set_password(app, &link, &ip, NEW_PASSWORD, "bl07-Something-Else-9").await;
        assert_eq!(mismatch.status(), StatusCode::OK);
        let html = body_text(mismatch).await;
        assert!(html.contains("Passwords do not match"), "{html}");
        assert!(
            html.contains(r#"name="password""#),
            "the form again: {html}"
        );

        let weak = set_password(app, &link, &ip, "short", "short").await;
        assert_eq!(weak.status(), StatusCode::OK);
        let html = body_text(weak).await;
        assert!(
            html.contains(r#"name="password""#),
            "the form again: {html}"
        );
        assert!(
            html.contains("message--error"),
            "the weak password error is shown: {html}"
        );

        assert_eq!(password_hash(app, user).await, before, "nothing changed");
        assert!(
            PasswordResetToken::find_valid(&app.db, &token)
                .await
                .unwrap()
                .is_some(),
            "the token is still valid"
        );
    });
}

/// Two posts of one token at once: exactly one changes the password.
#[test]
fn two_concurrent_posts_of_one_token_change_the_password_once() {
    common::run_test(async {
        let app = &fixture().app;
        let (user, _) = known_account(app, "bl07-race").await;
        let (link, _) = minted_link(app, user).await;
        let ip = ip_for(&format!("{user}-race"));

        // Two browsers, each with the form open and its own CSRF token.
        let (_, cookies_a, html_a) = get(app, &link, &ip, "").await;
        let (_, cookies_b, html_b) = get(app, &link, &ip, "").await;
        let token_a = csrf_token(&html_a);
        let token_b = csrf_token(&html_b);
        let first = "bl07-Race-Password-AAA1";
        let second = "bl07-Race-Password-BBB2";

        let fields_a = [
            ("_token", token_a.as_str()),
            ("password", first),
            ("confirm_password", first),
        ];
        let fields_b = [
            ("_token", token_b.as_str()),
            ("password", second),
            ("confirm_password", second),
        ];
        let (a, b) = tokio::join!(
            post_form(app, &link, &ip, &cookies_a, &fields_a),
            post_form(app, &link, &ip, &cookies_b, &fields_b),
        );

        let successes = [a.status(), b.status()]
            .iter()
            .filter(|s| s.is_redirection())
            .count();
        assert_eq!(successes, 1, "exactly one post may change the password");

        let audits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM password_reset_tokens WHERE user_id = $1 AND used_at IS NOT NULL",
        )
        .bind(user)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(audits, 1, "one token, used once");
    });
}
