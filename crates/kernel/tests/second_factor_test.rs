#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B304 — TOTP second factor: enrolment, the post-password verify step,
//! recovery-completion handling, a role's second-factor requirement, and the
//! admin reset.
//!
//! This file builds its **own** `TestApp` with `ENCRYPTION_KEY` configured,
//! because the shared app (`common::shared_app`) has none and TOTP enrolment
//! is unavailable without it. Codes are generated in these tests with the
//! same `totp-rs` crate the kernel uses, from the secret the enrolment page
//! shows — never by reaching into the kernel's sealed storage.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use totp_rs::{Algorithm, Builder, Secret};

/// The one app every test in this file shares, built once with an encryption
/// key configured. Mirrors `recovery_plugin_flow_test`'s own-app pattern
/// (`common::shared_app` cannot be reused: it has no `ENCRYPTION_KEY`, and
/// two concurrently-built `AppState`s would race on migrations).
static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

const TEST_ENCRYPTION_KEY: [u8; 32] = [0x42; 32];

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || {
            handle.block_on(TestApp::with_config(|config| {
                config.encryption_key = Some(TEST_ENCRYPTION_KEY);
            }))
        })
        .join()
        .expect("second-factor fixture app init thread panicked")
    })
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

async fn user_id_of(app: &TestApp, username: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// Clear any TOTP/recovery-code state so a fixed username is repeatable.
async fn reset_totp_state(app: &TestApp, user_id: uuid::Uuid) {
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1")
        .bind(user_id)
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM user_totp WHERE user_id = $1")
        .bind(user_id)
        .execute(&app.db)
        .await
        .unwrap();
}

/// Reset the lockout and per-IP login rate limit for a (username, ip) pair.
async fn reset_login_limits(app: &TestApp, username: &str, ip: &str) {
    app.state.lockout().clear_all(username).await.ok();
    app.state.rate_limiter().reset("login", ip).await.ok();
}

/// Extract a hand-written form's `_token` hidden field from rendered HTML
/// (the login, verify and two-factor pages all print it this way, not as a
/// `<meta>` tag, since several of them render for a visitor with no session).
fn token_field(html: &str) -> String {
    let marker = "name=\"_token\" value=\"";
    let start = html.find(marker).expect("a _token field") + marker.len();
    let end = start + html[start..].find('"').unwrap();
    html[start..end].to_string()
}

fn form_body(fields: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish()
}

/// A fresh user with no TOTP, logged out, with a distinct fake IP.
async fn fresh_user(app: &TestApp, username: &str) -> (uuid::Uuid, String) {
    app.create_test_user(
        username,
        "test-password-123",
        &format!("{username}@example.com"),
    )
    .await;
    let user_id = user_id_of(app, username).await;
    reset_totp_state(app, user_id).await;
    let ip = common::test_ip_for(username);
    reset_login_limits(app, username, &ip).await;
    (user_id, ip)
}

/// GET `path` and return (cookies, html). Keeps the caller's cookies when the
/// response sets none of its own (most `GET`s don't), the way
/// `RecoveryVisit::post` does — overwriting with an empty string would
/// silently drop the session for every later request in the chain.
async fn get(app: &TestApp, path: &str, cookies: &str, ip: &str) -> (String, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    let new_cookies = common::extract_cookies(&response);
    let cookies = if new_cookies.is_empty() {
        cookies.to_string()
    } else {
        new_cookies
    };
    let html = text_of(response).await;
    (cookies, html)
}

/// Build a TOTP instance from a base32 secret the way the kernel does
/// (`services::totp`): SHA-1, 6 digits, 30-second step, one step of skew.
fn totp_from_base32(secret: &str) -> totp_rs::Totp {
    let secret = Secret::try_from_base32(secret).expect("a valid base32 secret");
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(secret)
        .build()
        .expect("a valid TOTP instance")
}

fn now() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}

/// Log a user in by form (not JSON), following the one redirect this suite
/// cares about. Returns (status, location, cookies).
async fn login_form(
    app: &TestApp,
    cookies: &str,
    ip: &str,
    username: &str,
    password: &str,
) -> (StatusCode, Option<String>, String) {
    let (_, login_html) = get(app, "/user/login", cookies, ip).await;
    let token = token_field(&login_html);
    let response = app
        .request_with_cookies(
            Request::post("/user/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("x-forwarded-for", ip)
                .body(Body::from(form_body(&[
                    ("username", username),
                    ("password", password),
                    ("_token", &token),
                ])))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cookies = common::extract_cookies(&response);
    (status, location, cookies)
}

// ─── (a) Enrolment ───────────────────────────────────────────────────────────

#[test]
fn enrolment_shows_qr_and_secret_rejects_a_wrong_code_and_confirms_with_a_right_one() {
    common::run_test(async {
        let app = app();
        let username = "totp_enroll_a";
        let (user_id, ip) = fresh_user(app, username).await;
        let cookies = app.login(username, "test-password-123").await;

        // Start enrolment.
        let (_, status_html) = get(app, "/user/two-factor", &cookies, &ip).await;
        let start_token = token_field(&status_html);
        let start = app
            .request_with_cookies(
                Request::post("/user/two-factor/setup")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[("_token", &start_token)])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(start.status(), StatusCode::SEE_OTHER);

        let (cookies, setup_html) = get(app, "/user/two-factor/setup", &cookies, &ip).await;
        assert!(
            setup_html.contains("<svg") && setup_html.contains("<path"),
            "the setup page must show a QR code as inline SVG"
        );
        let manual_secret_marker = "<code>";
        let secret_start =
            setup_html.find(manual_secret_marker).expect("a manual secret") + manual_secret_marker.len();
        let secret_end = secret_start + setup_html[secret_start..].find("</code>").unwrap();
        let manual_secret = setup_html[secret_start..secret_end].to_string();
        let confirm_token = token_field(&setup_html);

        // A row exists, unconfirmed, and its secret is not the plain one.
        let stored_secret: Vec<u8> =
            sqlx::query_scalar("SELECT secret FROM user_totp WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_ne!(
            stored_secret,
            manual_secret.as_bytes(),
            "the stored secret must not be the plain secret"
        );
        let confirmed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT confirmed_at FROM user_totp WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(confirmed_at.is_none(), "a fresh enrolment is unconfirmed");

        // A wrong code is refused, and nothing is stored (still unconfirmed).
        let wrong = app
            .request_with_cookies(
                Request::post("/user/two-factor/confirm")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[
                        ("_token", &confirm_token),
                        ("code", "000000"),
                    ])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(wrong.status(), StatusCode::SEE_OTHER);
        let confirmed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT confirmed_at FROM user_totp WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(
            confirmed_at.is_none(),
            "a wrong confirmation code must not confirm the enrolment"
        );
        let recovery_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_recovery_codes WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(recovery_count, 0, "no recovery codes on a failed confirm");

        // A right code confirms it and issues ten recovery codes, shown once.
        let totp = totp_from_base32(&manual_secret);
        let right_code = totp.generate(now()).to_string();
        let (_, confirm_html) = get(app, "/user/two-factor/setup", &cookies, &ip).await;
        let confirm_token = token_field(&confirm_html);
        let confirm = app
            .request_with_cookies(
                Request::post("/user/two-factor/confirm")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[
                        ("_token", &confirm_token),
                        ("code", &right_code),
                    ])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(confirm.status(), StatusCode::SEE_OTHER);

        let (cookies, codes_html) = get(app, "/user/two-factor/recovery-codes", &cookies, &ip).await;
        let mut shown_codes = Vec::new();
        for line in codes_html.lines() {
            if let Some(start) = line.find("<code>") {
                let start = start + "<code>".len();
                if let Some(end) = line[start..].find("</code>") {
                    shown_codes.push(line[start..start + end].to_string());
                }
            }
        }
        assert_eq!(shown_codes.len(), 10, "ten recovery codes must be shown");

        let hashes: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT code_hash FROM user_recovery_codes WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert_eq!(hashes.len(), 10);
        for code in &shown_codes {
            assert!(
                !hashes.contains(&code.replace('-', "").into_bytes()),
                "a recovery code must never be stored in the clear"
            );
        }

        let confirmed_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT confirmed_at FROM user_totp WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(confirmed_at.is_some(), "a right code confirms the enrolment");

        // Shown once: visiting again finds nothing left to show.
        let second_visit = app
            .request_with_cookies(
                Request::get("/user/two-factor/recovery-codes")
                    .header("x-forwarded-for", &ip)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(second_visit.status(), StatusCode::SEE_OTHER);
    });
}

// ─── (b) Password login with TOTP enrolled ─────────────────────────────────

/// Enroll `username` (already logged in via `cookies`) and return the TOTP
/// instance to generate codes with.
async fn enroll_totp(app: &TestApp, cookies: &str, ip: &str) -> totp_rs::Totp {
    let (_, status_html) = get(app, "/user/two-factor", cookies, ip).await;
    let start_token = token_field(&status_html);
    app.request_with_cookies(
        Request::post("/user/two-factor/setup")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", ip)
            .body(Body::from(form_body(&[("_token", &start_token)])))
            .unwrap(),
        cookies,
    )
    .await;
    let (cookies, setup_html) = get(app, "/user/two-factor/setup", cookies, ip).await;
    let marker = "<code>";
    let secret_start = setup_html.find(marker).unwrap() + marker.len();
    let secret_end = secret_start + setup_html[secret_start..].find("</code>").unwrap();
    let manual_secret = setup_html[secret_start..secret_end].to_string();
    let totp = totp_from_base32(&manual_secret);
    let confirm_token = token_field(&setup_html);
    let code = totp.generate(now()).to_string();
    let confirm = app
        .request_with_cookies(
            Request::post("/user/two-factor/confirm")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("x-forwarded-for", ip)
                .body(Body::from(form_body(&[
                    ("_token", &confirm_token),
                    ("code", &code),
                ])))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(confirm.status(), StatusCode::SEE_OTHER, "enrolment must confirm");
    totp
}

async fn recovery_codes_of(app: &TestApp, cookies: &str, ip: &str) -> Vec<String> {
    let (_, html) = get(app, "/user/two-factor/recovery-codes", cookies, ip).await;
    let mut codes = Vec::new();
    for line in html.lines() {
        if let Some(start) = line.find("<code>") {
            let start = start + "<code>".len();
            if let Some(end) = line[start..].find("</code>") {
                codes.push(line[start..start + end].to_string());
            }
        }
    }
    codes
}

#[test]
fn password_login_for_a_totp_enrolled_account_pends_until_a_right_code_is_given() {
    common::run_test(async {
        let app = app();
        let username = "totp_login_b";
        let (_user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        let totp = enroll_totp(app, &setup_cookies, &ip).await;

        // A fresh (logged-out) password login.
        let (status, location, pending_cookies) =
            login_form(app, "", &ip, username, "test-password-123").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location.as_deref(), Some("/user/login/verify"));

        // No session yet.
        let profile = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &pending_cookies,
            )
            .await;
        assert_eq!(
            profile.status(),
            StatusCode::UNAUTHORIZED,
            "the password alone must not establish a session for a TOTP-enrolled account"
        );

        // The verify page itself works.
        let (cookies, verify_html) = get(app, "/user/login/verify", &pending_cookies, &ip).await;
        let token = token_field(&verify_html);
        let code = totp.generate(now()).to_string();
        let verify = app
            .request_with_cookies(
                Request::post("/user/login/verify")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[("_token", &token), ("code", &code)])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(verify.status(), StatusCode::SEE_OTHER);
        let session_cookies = common::extract_cookies(&verify);
        assert!(!session_cookies.is_empty());

        let profile = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &session_cookies,
            )
            .await;
        assert_eq!(profile.status(), StatusCode::OK, "a right code completes sign-in");

        let kind: String = sqlx::query_scalar(
            "SELECT kind FROM security_audit_log WHERE user_id = \
             (SELECT id FROM users WHERE name = $1) ORDER BY created DESC LIMIT 1",
        )
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(kind, "auth.login_succeeded");
    });
}

#[test]
fn a_wrong_verify_code_is_refused_and_counts_toward_the_account_lockout() {
    common::run_test(async {
        let app = app();
        let username = "totp_login_wrong";
        let (_user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        enroll_totp(app, &setup_cookies, &ip).await;

        let (status, _loc, pending_cookies) =
            login_form(app, "", &ip, username, "test-password-123").await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        for _ in 0..5 {
            let (cookies, verify_html) = get(app, "/user/login/verify", &pending_cookies, &ip).await;
            let token = token_field(&verify_html);
            let wrong = app
                .request_with_cookies(
                    Request::post("/user/login/verify")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .header("x-forwarded-for", &ip)
                        .body(Body::from(form_body(&[
                            ("_token", &token),
                            ("code", "000000"),
                        ])))
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(wrong.status(), StatusCode::OK, "a wrong code re-renders the verify page");
        }

        assert!(
            app.state.lockout().is_locked(username, &ip).await.unwrap(),
            "five wrong second-factor codes must lock the account out, as five wrong passwords do"
        );
    });
}

#[test]
fn the_same_totp_code_cannot_be_used_twice() {
    common::run_test(async {
        let app = app();
        let username = "totp_login_replay";
        let (_user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        let totp = enroll_totp(app, &setup_cookies, &ip).await;

        let (_status, _loc, pending_cookies) =
            login_form(app, "", &ip, username, "test-password-123").await;
        let (cookies, verify_html) = get(app, "/user/login/verify", &pending_cookies, &ip).await;
        let token = token_field(&verify_html);
        let code = totp.generate(now()).to_string();

        let first = app
            .request_with_cookies(
                Request::post("/user/login/verify")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[("_token", &token), ("code", &code)])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(first.status(), StatusCode::SEE_OTHER, "the first use must succeed");

        // Log out and start a second pending verify with the SAME code.
        reset_login_limits(app, username, &ip).await;
        let (_status, _loc, pending_cookies) =
            login_form(app, "", &ip, username, "test-password-123").await;
        let (cookies, verify_html) = get(app, "/user/login/verify", &pending_cookies, &ip).await;
        let token = token_field(&verify_html);
        let replay = app
            .request_with_cookies(
                Request::post("/user/login/verify")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[("_token", &token), ("code", &code)])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            replay.status(),
            StatusCode::OK,
            "the same code must be refused the second time (re-renders with an error)"
        );
    });
}

#[test]
fn a_recovery_code_works_once_and_never_again() {
    common::run_test(async {
        let app = app();
        let username = "totp_login_recovery";
        let (_user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        enroll_totp(app, &setup_cookies, &ip).await;
        let codes = recovery_codes_of(app, &setup_cookies, &ip).await;
        let code = codes[0].clone();

        let (_status, _loc, pending_cookies) =
            login_form(app, "", &ip, username, "test-password-123").await;
        let (cookies, verify_html) = get(app, "/user/login/verify", &pending_cookies, &ip).await;
        let token = token_field(&verify_html);
        let first = app
            .request_with_cookies(
                Request::post("/user/login/verify")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[("_token", &token), ("code", &code)])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(first.status(), StatusCode::SEE_OTHER);

        reset_login_limits(app, username, &ip).await;
        let (_status, _loc, pending_cookies) =
            login_form(app, "", &ip, username, "test-password-123").await;
        let (cookies, verify_html) = get(app, "/user/login/verify", &pending_cookies, &ip).await;
        let token = token_field(&verify_html);
        let second = app
            .request_with_cookies(
                Request::post("/user/login/verify")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(form_body(&[("_token", &token), ("code", &code)])))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            second.status(),
            StatusCode::OK,
            "a spent recovery code must never verify a second time"
        );
    });
}

// ─── (c) JSON login ──────────────────────────────────────────────────────────

#[test]
fn json_login_without_a_code_asks_for_one_and_a_right_code_succeeds() {
    common::run_test(async {
        let app = app();
        let username = "totp_login_json";
        let (_user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        let totp = enroll_totp(app, &setup_cookies, &ip).await;
        reset_login_limits(app, username, &ip).await;

        let without_code = app
            .request(
                Request::post("/user/login/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({
                            "username": username,
                            "password": "test-password-123",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(without_code.status(), StatusCode::UNAUTHORIZED);
        let body = json_body(without_code).await;
        assert!(
            body.to_string().contains("second_factor_required"),
            "the body must say second_factor_required: {body}"
        );

        let code = totp.generate(now()).to_string();
        let with_code = app
            .request(
                Request::post("/user/login/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({
                            "username": username,
                            "password": "test-password-123",
                            "code": code,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(with_code.status(), StatusCode::OK);
    });
}

// ─── (d) Account recovery for a TOTP-enrolled account ──────────────────────

fn recovery_ip_for(tag: &str) -> String {
    let n: u32 = tag.bytes().map(u32::from).sum();
    format!("10.91.{}.{}", (n / 250) % 250, n % 250 + 1)
}

#[test]
fn completed_recovery_for_a_totp_enrolled_account_lands_on_verify_not_a_session() {
    common::run_test(async {
        let app = app();
        let username = "totp_recovery_d";
        let (_user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        enroll_totp(app, &setup_cookies, &ip).await;

        let user_id = user_id_of(app, username).await;
        let recovery_ip = recovery_ip_for(username);
        app.state
            .rate_limiter()
            .reset("recovery", &recovery_ip)
            .await
            .ok();
        app.state
            .rate_limiter()
            .reset("recovery", &format!("user:{user_id}"))
            .await
            .ok();
        let account_codes =
            trovato_kernel::services::recovery_builtins::RecoveryCodesProvider::generate(
                &app.db, user_id,
            )
            .await
            .expect("account recovery codes should generate");

        let mut flow = common::RecoveryVisit::open(app, &recovery_ip).await;
        let start = flow
            .post(
                app,
                "/user/recover/start",
                serde_json::json!({ "identifier": username }).to_string(),
            )
            .await;
        assert_eq!(start.status(), StatusCode::OK);

        let choose = flow
            .post(
                app,
                "/user/recover/choose",
                serde_json::json!({ "method_id": "trovato_recovery_codes:code" }).to_string(),
            )
            .await;
        assert_eq!(choose.status(), StatusCode::OK);

        let verify = flow
            .post(
                app,
                "/user/recover/verify",
                serde_json::json!({ "response": account_codes[0] }).to_string(),
            )
            .await;
        assert_eq!(verify.status(), StatusCode::OK);

        let reset = flow
            .post(
                app,
                "/user/recover/reset",
                serde_json::json!({ "new_password": "brand-new-password-totp-1" }).to_string(),
            )
            .await;
        assert_eq!(reset.status(), StatusCode::OK);
        assert_eq!(
            json_body(reset).await["next"],
            "second_factor",
            "a TOTP-enrolled account's completed recovery must say so, not grant a session"
        );

        let not_logged_in = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &flow.cookies,
            )
            .await;
        assert_eq!(
            not_logged_in.status(),
            StatusCode::UNAUTHORIZED,
            "recovery must not hand a TOTP-enrolled account a session on its own"
        );
    });
}

// ─── (e) A role requiring a second factor ──────────────────────────────────

#[test]
fn a_role_requiring_a_second_factor_forces_enrolment_until_satisfied() {
    common::run_test(async {
        use trovato_kernel::models::Role;

        let app = app();
        let username = "totp_role_required_e";
        let (user_id, ip) = fresh_user(app, username).await;

        let role_id = common::create_test_role(app, "totp-required-role-e").await;
        Role::set_requires_second_factor(&app.db, role_id, true)
            .await
            .unwrap();
        Role::assign_to_user(&app.db, user_id, role_id)
            .await
            .unwrap();
        app.state.permissions().invalidate_user(user_id);

        let cookies = app.login(username, "test-password-123").await;

        // Redirected to enrolment from `/`, from an admin page, and from the
        // profile page.
        for path in ["/", "/admin", "/user/profile"] {
            let response = app
                .request_with_cookies(
                    Request::get(path)
                        .header("x-forwarded-for", &ip)
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::SEE_OTHER,
                "{path} must redirect to enrolment"
            );
            assert_eq!(
                response.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()),
                Some("/user/two-factor")
            );
        }

        // 403 for a JSON route.
        let json_route = app
            .request_with_cookies(
                Request::get("/api/v1/items")
                    .header("x-forwarded-for", &ip)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(json_route.status(), StatusCode::FORBIDDEN);

        // Logging out and the two-factor pages themselves still work.
        let (_, status_html) = get(app, "/user/two-factor", &cookies, &ip).await;
        assert!(status_html.contains("Set up an authenticator"));

        // Enrolling satisfies the requirement.
        enroll_totp(app, &cookies, &ip).await;
        let free = app
            .request_with_cookies(
                Request::get("/")
                    .header("x-forwarded-for", &ip)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            free.status(),
            StatusCode::OK,
            "the account is free to use the site once enrolled"
        );
    });
}

#[test]
fn a_passkey_only_account_in_a_required_role_is_refused_a_password_login() {
    common::run_test(async {
        use trovato_kernel::models::Role;
        use webauthn_authenticator_rs::WebauthnAuthenticator;
        use webauthn_authenticator_rs::softpasskey::SoftPasskey;

        let app = app();
        let username = "totp_passkey_only_e";
        let (user_id, ip) = fresh_user(app, username).await;
        sqlx::query("DELETE FROM webauthn_credentials WHERE user_id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();

        let role_id = common::create_test_role(app, "totp-required-role-passkey-e").await;
        Role::set_requires_second_factor(&app.db, role_id, true)
            .await
            .unwrap();
        Role::assign_to_user(&app.db, user_id, role_id)
            .await
            .unwrap();
        app.state.permissions().invalidate_user(user_id);

        let cookies = app.login(username, "test-password-123").await;

        // Register a passkey through the real ceremony.
        let csrf = {
            let (_, html) = get(app, "/user/two-factor", &cookies, &ip).await;
            token_field(&html)
        };
        let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let start = app
            .request_with_cookies(
                Request::post("/user/webauthn/register/start")
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(start.status(), StatusCode::OK);
        let started = json_body(start).await;
        let finish_csrf = started["csrf_token"].as_str().unwrap().to_string();
        let rp_origin = app
            .state
            .webauthn()
            .expect("webauthn must be configured")
            .get_allowed_origins()
            .first()
            .unwrap()
            .clone();
        let challenge: webauthn_rs::prelude::CreationChallengeResponse =
            serde_json::from_value(started["options"].clone()).unwrap();
        let credential = authenticator.do_registration(rp_origin, challenge).unwrap();
        let finish = app
            .request_with_cookies(
                Request::post("/user/webauthn/register/finish")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &finish_csrf)
                    .body(Body::from(
                        serde_json::json!({ "credential": credential, "device_name": "Only device" })
                            .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(finish.status(), StatusCode::OK);

        // Now a password login must be refused: this account signs in with
        // its passkey.
        reset_login_limits(app, username, &ip).await;
        let (status, _location, _cookies) =
            login_form(app, "", &ip, username, "test-password-123").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    });
}

// ─── (g) Admin reset ────────────────────────────────────────────────────────

#[test]
fn an_admin_can_reset_another_users_second_factor() {
    common::run_test(async {
        let app = app();
        let username = "totp_reset_target_g";
        let (user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        enroll_totp(app, &setup_cookies, &ip).await;
        assert!(
            trovato_kernel::services::second_factor::has_confirmed_totp(&app.state, user_id).await
        );

        let (admin_id, admin_cookies) = common::user_holding(app, "totp_reset_admin_g", &["administer users"]).await;
        let admin_ip = common::test_ip_for(&format!("admin-{admin_id}"));

        let (_, edit_html) = get(
            app,
            &format!("/admin/people/{user_id}/edit"),
            &admin_cookies,
            &admin_ip,
        )
        .await;
        assert!(edit_html.contains("Reset second factor"));

        // The main edit form's own `action="/admin/people/..."` appears first
        // on the page, so anchor on the reset form's distinguishing text
        // (`second-factor/reset`) rather than the shared `/admin/people/`
        // prefix, then take the first `_token` after that point.
        let reset_marker = "second-factor/reset";
        let reset_pos = edit_html.find(reset_marker).expect("the reset form's action");
        let after_reset = &edit_html[reset_pos..];
        let token_marker = "name=\"_token\" value=\"";
        let token_pos = after_reset
            .find(token_marker)
            .expect("the reset form's token");
        let token_start = reset_pos + token_pos + token_marker.len();
        let token_end = token_start + edit_html[token_start..].find('"').unwrap();
        let token = edit_html[token_start..token_end].to_string();

        let reset = app
            .request_with_cookies(
                Request::post(format!("/admin/people/{user_id}/second-factor/reset"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &admin_ip)
                    .body(Body::from(form_body(&[("_token", &token)])))
                    .unwrap(),
                &admin_cookies,
            )
            .await;
        assert_eq!(reset.status(), StatusCode::SEE_OTHER);

        assert!(
            !trovato_kernel::services::second_factor::has_confirmed_totp(&app.state, user_id).await,
            "the factor must be gone"
        );
        let recovery_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_recovery_codes WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(recovery_count, 0, "recovery codes must be gone too");

        // The user's session was ended.
        let still_signed_in = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &setup_cookies,
            )
            .await;
        assert_eq!(still_signed_in.status(), StatusCode::UNAUTHORIZED);

        let kind: String = sqlx::query_scalar(
            "SELECT kind FROM security_audit_log WHERE user_id = $1 AND kind = 'totp.reset_by_admin' \
             ORDER BY created DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(kind, "totp.reset_by_admin");
    });
}

#[test]
fn a_user_without_the_permission_cannot_reset_someone_elses_second_factor() {
    common::run_test(async {
        let app = app();
        let username = "totp_reset_forbidden_g";
        let (user_id, ip) = fresh_user(app, username).await;
        let setup_cookies = app.login(username, "test-password-123").await;
        enroll_totp(app, &setup_cookies, &ip).await;

        let (_other_id, other_cookies) =
            common::user_holding(app, "totp_reset_nobody_g", &[]).await;
        let other_ip = common::test_ip_for("totp_reset_nobody_g_ip");

        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/people/{user_id}/second-factor/reset"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &other_ip)
                    .body(Body::from(form_body(&[("_token", "irrelevant")])))
                    .unwrap(),
                &other_cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a signed-in user without administer users must be refused, not redirected"
        );
        assert!(
            trovato_kernel::services::second_factor::has_confirmed_totp(&app.state, user_id).await,
            "the factor must still be there"
        );
    });
}
