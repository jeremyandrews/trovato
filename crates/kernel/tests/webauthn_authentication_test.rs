#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Story 4.2 — WebAuthn authentication ceremony, end to end at the HTTP layer.
//!
//! Same posture as the registration tests: a real `SoftPasskey` signs a real
//! assertion against the kernel's real relying party, and `webauthn-rs` really
//! verifies it. What is asserted here beyond "it works" is the security
//! behaviour: the fixation invariant (AC-2), the D-37 counter-regression
//! disposition (AC-3), and that every rejection names its specific cause rather
//! than only failing.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn rp_origin(app: &TestApp) -> url::Url {
    app.state
        .webauthn()
        .get_allowed_origins()
        .first()
        .expect("the relying party must have an origin")
        .clone()
}

async fn csrf_token(app: &TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/user/passkeys").body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    let body = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    let pos = html.find("csrf-token").expect("csrf-token meta tag");
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

/// Register a passkey for a logged-in user, returning the authenticator that
/// holds it so the same soft device can later authenticate.
async fn register_passkey(app: &TestApp, cookies: &str) -> WebauthnAuthenticator<SoftPasskey> {
    let csrf = csrf_token(app, cookies).await;
    let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));

    let start = app
        .request_with_cookies(
            Request::post("/user/webauthn/register/start")
                .header("x-csrf-token", &csrf)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(start.status(), StatusCode::OK);
    let started = json_body(start).await;
    let finish_csrf = started["csrf_token"].as_str().unwrap().to_string();
    let challenge: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(started["options"].clone()).unwrap();

    let credential = authenticator
        .do_registration(rp_origin(app), challenge)
        .unwrap();

    let finish = app
        .request_with_cookies(
            Request::post("/user/webauthn/register/finish")
                .header("content-type", "application/json")
                .header("x-csrf-token", &finish_csrf)
                .body(Body::from(
                    serde_json::json!({ "credential": credential }).to_string(),
                ))
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(finish.status(), StatusCode::OK, "setup registration failed");
    authenticator
}

/// Drive the full login ceremony from a clean (anonymous) session.
///
/// Returns the `/login/finish` response and the **post-ceremony** cookies. Those
/// are the cookies from `/finish`, not the ones the ceremony ran under: `finish`
/// goes through `setup_session`, whose `cycle_id` rotates the session id, so the
/// pre-ceremony cookie is deliberately dead afterwards (see
/// `passkey_login_cycles_the_session_id`).
async fn passkey_login(
    app: &TestApp,
    username: &str,
    authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
) -> (axum::response::Response, String) {
    let fake_ip = format!("10.90.{}.{}", username.len(), username.len() % 250 + 1);
    app.state.rate_limiter().reset("login", &fake_ip).await.ok();

    let start = app
        .request(
            Request::post("/user/webauthn/login/start")
                .header("content-type", "application/json")
                .header("x-forwarded-for", &fake_ip)
                .body(Body::from(
                    serde_json::json!({ "username": username }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        start.status(),
        StatusCode::OK,
        "login/start should succeed for an account with a passkey"
    );
    let cookies = common::extract_cookies(&start);
    let challenge: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(json_body(start).await).expect("valid request challenge");

    let assertion = authenticator
        .do_authentication(rp_origin(app), challenge)
        .expect("the soft authenticator should sign the assertion");

    let finish = app
        .request_with_cookies(
            Request::post("/user/webauthn/login/finish")
                .header("content-type", "application/json")
                .header("x-forwarded-for", &fake_ip)
                .body(Body::from(
                    serde_json::json!({ "credential": assertion }).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    let post_cookies = common::extract_cookies(&finish);
    (finish, post_cookies)
}

async fn user_id_of(app: &TestApp, username: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

#[test]
fn authenticates_with_a_registered_passkey() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "wa_auth_ok";
        let setup_cookies = app
            .create_and_login_user(username, "test-password-123", "wa_auth_ok@example.com")
            .await;
        let mut authenticator = register_passkey(app, &setup_cookies).await;

        let (finish, login_cookies) = passkey_login(app, username, &mut authenticator).await;
        assert_eq!(
            finish.status(),
            StatusCode::OK,
            "passkey login should succeed"
        );
        assert_eq!(json_body(finish).await["success"], true);

        // The resulting session is a real authenticated session: it can reach a
        // login-gated page. These are the post-`cycle_id` cookies from `/finish`.
        let profile = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &login_cookies,
            )
            .await;
        assert!(
            profile.status() == StatusCode::OK,
            "the passkey session should be authenticated, got {}",
            profile.status()
        );

        let user_id = user_id_of(app, username).await;

        // AC-3: a successful authentication stamps last_used_at.
        let last_used: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT last_used_at FROM webauthn_credentials WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(
            last_used.is_some(),
            "a successful authentication must record last_used_at"
        );

        // AC-5 (4.1's module, this story's emitter): the login is audited with
        // its method, so "which factor was used" is answerable after the fact.
        let method: Option<String> = sqlx::query_scalar(
            "SELECT details->>'method' FROM security_audit_log
             WHERE user_id = $1 AND kind = 'auth.login_succeeded'
             ORDER BY created DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
        .flatten();
        assert_eq!(method.as_deref(), Some("passkey"));
    });
}

#[test]
fn passkey_login_cycles_the_session_id() {
    common::run_test(async {
        // AC-2, the fixation invariant. The session that carried the ceremony
        // must not be the session that comes out authenticated.
        let app = common::shared_app().await;
        let username = "wa_auth_fixation";
        let setup_cookies = app
            .create_and_login_user(
                username,
                "test-password-123",
                "wa_auth_fixation@example.com",
            )
            .await;
        let mut authenticator = register_passkey(app, &setup_cookies).await;

        let fake_ip = "10.91.0.7";
        app.state.rate_limiter().reset("login", fake_ip).await.ok();

        let start = app
            .request(
                Request::post("/user/webauthn/login/start")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(
                        serde_json::json!({ "username": username }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(start.status(), StatusCode::OK);
        let pre_ceremony_cookies = common::extract_cookies(&start);
        let challenge: webauthn_rs::prelude::RequestChallengeResponse =
            serde_json::from_value(json_body(start).await).unwrap();
        let assertion = authenticator
            .do_authentication(rp_origin(app), challenge)
            .unwrap();

        let finish = app
            .request_with_cookies(
                Request::post("/user/webauthn/login/finish")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(
                        serde_json::json!({ "credential": assertion }).to_string(),
                    ))
                    .unwrap(),
                &pre_ceremony_cookies,
            )
            .await;
        assert_eq!(finish.status(), StatusCode::OK);

        // `setup_session` calls `cycle_id`, so `/finish` must re-issue the cookie
        // with a different id than the ceremony ran under.
        let post_cookies = common::extract_cookies(&finish);
        assert!(
            !post_cookies.is_empty(),
            "finish must Set-Cookie a rotated session"
        );
        assert_ne!(
            post_cookies, pre_ceremony_cookies,
            "session id must be cycled after the auth-state change (fixation invariant)"
        );
    });
}

#[test]
fn passkey_login_writes_the_same_principal_as_password_login() {
    common::run_test(async {
        // FINDING-A4: a passkey login writes the same SESSION_USER_ID as a
        // password login. One principal, not a second auth-principal type.
        let app = common::shared_app().await;
        let username = "wa_auth_principal";
        let setup_cookies = app
            .create_and_login_user(
                username,
                "test-password-123",
                "wa_auth_principal@example.com",
            )
            .await;
        let mut authenticator = register_passkey(app, &setup_cookies).await;

        let (finish, cookies) = passkey_login(app, username, &mut authenticator).await;
        assert_eq!(finish.status(), StatusCode::OK);

        // The profile page renders this account's own data — proof the session
        // resolves to the same user a password login would.
        let profile = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(profile.status(), StatusCode::OK);
        let body = axum::body::to_bytes(profile.into_body(), 2_000_000)
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains(username),
            "the passkey session must resolve to the same account"
        );
    });
}

#[test]
fn login_start_is_generic_for_an_unknown_account() {
    common::run_test(async {
        // Enumeration posture: an account that does not exist and an account
        // with no passkey must be indistinguishable to the caller.
        let app = common::shared_app().await;

        app.state
            .rate_limiter()
            .reset("login", "10.92.0.1")
            .await
            .ok();
        let unknown = app
            .request(
                Request::post("/user/webauthn/login/start")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", "10.92.0.1")
                    .body(Body::from(
                        serde_json::json!({ "username": "no_such_account_at_all" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;

        let username = "wa_auth_nopasskey";
        app.create_test_user(username, "test-password-123", "wa_auth_nopk@example.com")
            .await;
        app.state
            .rate_limiter()
            .reset("login", "10.92.0.2")
            .await
            .ok();
        let no_passkey = app
            .request(
                Request::post("/user/webauthn/login/start")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", "10.92.0.2")
                    .body(Body::from(
                        serde_json::json!({ "username": username }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;

        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            no_passkey.status(),
            StatusCode::UNAUTHORIZED,
            "a known account with no passkey must look exactly like an unknown one"
        );
        let a = json_body(unknown).await;
        let b = json_body(no_passkey).await;
        assert_eq!(a, b, "the two responses must be byte-identical");
    });
}

#[test]
fn login_finish_without_a_ceremony_is_rejected_and_audited() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "wa_auth_noceremony";
        let setup_cookies = app
            .create_and_login_user(
                username,
                "test-password-123",
                "wa_auth_noceremony@example.com",
            )
            .await;
        let mut authenticator = register_passkey(app, &setup_cookies).await;

        // Produce a genuinely signed assertion, then submit it against a session
        // that has no in-flight ceremony.
        let fake_ip = "10.93.0.1";
        app.state.rate_limiter().reset("login", fake_ip).await.ok();
        let start = app
            .request(
                Request::post("/user/webauthn/login/start")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(
                        serde_json::json!({ "username": username }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        let challenge: webauthn_rs::prelude::RequestChallengeResponse =
            serde_json::from_value(json_body(start).await).unwrap();
        let assertion = authenticator
            .do_authentication(rp_origin(app), challenge)
            .unwrap();

        // No cookies ⇒ a fresh session with no stored PasskeyAuthentication.
        let finish = app
            .request(
                Request::post("/user/webauthn/login/finish")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(
                        serde_json::json!({ "credential": assertion }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            finish.status(),
            StatusCode::BAD_REQUEST,
            "an assertion with no matching ceremony must be refused"
        );

        let reason: Option<String> = sqlx::query_scalar(
            "SELECT details->>'reason' FROM security_audit_log
             WHERE kind = 'auth.login_failed' AND details->>'method' = 'passkey'
               AND details->>'reason' = 'no_ceremony_in_progress'
             ORDER BY created DESC LIMIT 1",
        )
        .fetch_optional(&app.db)
        .await
        .unwrap()
        .flatten();
        assert_eq!(
            reason.as_deref(),
            Some("no_ceremony_in_progress"),
            "the specific rejection reason must be audited"
        );
    });
}

#[test]
fn a_replayed_assertion_is_rejected() {
    common::run_test(async {
        // The ceremony is consumed at /finish, so the same signed assertion
        // cannot be submitted twice.
        let app = common::shared_app().await;
        let username = "wa_auth_replay";
        let setup_cookies = app
            .create_and_login_user(username, "test-password-123", "wa_auth_replay@example.com")
            .await;
        let mut authenticator = register_passkey(app, &setup_cookies).await;

        let fake_ip = "10.94.0.1";
        app.state.rate_limiter().reset("login", fake_ip).await.ok();
        let start = app
            .request(
                Request::post("/user/webauthn/login/start")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(
                        serde_json::json!({ "username": username }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        let cookies = common::extract_cookies(&start);
        let challenge: webauthn_rs::prelude::RequestChallengeResponse =
            serde_json::from_value(json_body(start).await).unwrap();
        let assertion = authenticator
            .do_authentication(rp_origin(app), challenge)
            .unwrap();
        let payload = serde_json::json!({ "credential": assertion }).to_string();

        let first = app
            .request_with_cookies(
                Request::post("/user/webauthn/login/finish")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(payload.clone()))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(first.status(), StatusCode::OK);

        let replay = app
            .request_with_cookies(
                Request::post("/user/webauthn/login/finish")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(payload))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            replay.status(),
            StatusCode::BAD_REQUEST,
            "a replayed assertion has no ceremony left to verify against"
        );
    });
}

#[test]
fn a_counter_regression_is_rejected_and_flags_without_revoking() {
    common::run_test(async {
        // D-37 in full: reject the authentication, FLAG the credential, do NOT
        // auto-revoke, and audit the disposition.
        //
        // A SoftPasskey reports counter 0 forever (like a real platform
        // passkey), so a regression cannot be produced by signing. We instead
        // raise the STORED counter above what the authenticator will ever
        // present — which is exactly the cloned-authenticator signature the rule
        // exists to catch — and drive a real ceremony into it.
        let app = common::shared_app().await;
        let username = "wa_auth_counter";
        let setup_cookies = app
            .create_and_login_user(username, "test-password-123", "wa_auth_counter@example.com")
            .await;
        let user_id = user_id_of(app, username).await;
        // The integration DB persists across runs and this test uses a fixed
        // username, so start from a known-empty credential set.
        sqlx::query("DELETE FROM webauthn_credentials WHERE user_id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();
        let mut authenticator = register_passkey(app, &setup_cookies).await;

        let credential_row_id: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM webauthn_credentials WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();

        // Raise both the denormalized column and the authoritative blob, so the
        // library and the kernel both see a counter the assertion cannot beat.
        sqlx::query(
            "UPDATE webauthn_credentials
                SET sign_count = 50,
                    passkey_json = jsonb_set(passkey_json, '{cred,counter}', '50'::jsonb)
              WHERE id = $1",
        )
        .bind(credential_row_id)
        .execute(&app.db)
        .await
        .unwrap();

        let (finish, _cookies) = {
            let fake_ip = "10.95.0.1";
            app.state.rate_limiter().reset("login", fake_ip).await.ok();
            let start = app
                .request(
                    Request::post("/user/webauthn/login/start")
                        .header("content-type", "application/json")
                        .header("x-forwarded-for", fake_ip)
                        .body(Body::from(
                            serde_json::json!({ "username": username }).to_string(),
                        ))
                        .unwrap(),
                )
                .await;
            assert_eq!(start.status(), StatusCode::OK);
            let cookies = common::extract_cookies(&start);
            let challenge: webauthn_rs::prelude::RequestChallengeResponse =
                serde_json::from_value(json_body(start).await).unwrap();
            let assertion = authenticator
                .do_authentication(rp_origin(app), challenge)
                .unwrap();
            let finish = app
                .request_with_cookies(
                    Request::post("/user/webauthn/login/finish")
                        .header("content-type", "application/json")
                        .header("x-forwarded-for", fake_ip)
                        .body(Body::from(
                            serde_json::json!({ "credential": assertion }).to_string(),
                        ))
                        .unwrap(),
                    &cookies,
                )
                .await;
            (finish, cookies)
        };

        assert_eq!(
            finish.status(),
            StatusCode::UNAUTHORIZED,
            "an assertion whose counter did not advance must be rejected"
        );

        // Flagged...
        let (flagged_at, flag_reason, still_present): (
            Option<chrono::DateTime<chrono::Utc>>,
            Option<String>,
            i64,
        ) = {
            let row: (Option<chrono::DateTime<chrono::Utc>>, Option<String>) = sqlx::query_as(
                "SELECT flagged_at, flag_reason FROM webauthn_credentials WHERE id = $1",
            )
            .bind(credential_row_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_credentials WHERE id = $1")
                    .bind(credential_row_id)
                    .fetch_one(&app.db)
                    .await
                    .unwrap();
            (row.0, row.1, count)
        };
        assert!(flagged_at.is_some(), "the credential must be flagged");
        assert!(
            flag_reason
                .as_deref()
                .is_some_and(|r| r.contains("counter")),
            "the flag must say why: {flag_reason:?}"
        );

        // ...and NOT revoked. This is the whole point of D-37: a false positive
        // on auto-revoke is a self-inflicted lockout.
        assert_eq!(
            still_present, 1,
            "a counter regression must NOT delete the credential (D-37: flag, never auto-revoke)"
        );

        // Audited with the disposition spelled out.
        let disposition: Option<String> = sqlx::query_scalar(
            "SELECT details->>'disposition' FROM security_audit_log
             WHERE user_id = $1 AND kind = 'passkey.counter_regression'
             ORDER BY created DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
        .flatten();
        assert_eq!(
            disposition.as_deref(),
            Some("rejected_and_flagged_not_revoked")
        );
    });
}

#[test]
fn another_accounts_credential_cannot_complete_this_flow() {
    common::run_test(async {
        // The ceremony is bound to the account it was started for. Even a
        // genuinely signed assertion from a different account's authenticator
        // must not authenticate.
        let app = common::shared_app().await;

        let victim = "wa_auth_victim";
        let victim_cookies = app
            .create_and_login_user(victim, "test-password-123", "wa_auth_victim@example.com")
            .await;
        let _victim_auth = register_passkey(app, &victim_cookies).await;

        let attacker = "wa_auth_attacker";
        let attacker_cookies = app
            .create_and_login_user(
                attacker,
                "test-password-123",
                "wa_auth_attacker@example.com",
            )
            .await;
        let mut attacker_auth = register_passkey(app, &attacker_cookies).await;

        // Start a ceremony for the VICTIM, sign it with the ATTACKER's device.
        let fake_ip = "10.96.0.1";
        app.state.rate_limiter().reset("login", fake_ip).await.ok();
        let start = app
            .request(
                Request::post("/user/webauthn/login/start")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", fake_ip)
                    .body(Body::from(
                        serde_json::json!({ "username": victim }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(start.status(), StatusCode::OK);
        let cookies = common::extract_cookies(&start);
        let challenge: webauthn_rs::prelude::RequestChallengeResponse =
            serde_json::from_value(json_body(start).await).unwrap();

        // The attacker's device holds no credential in the victim's allow-list,
        // so it cannot even produce an assertion. That refusal is itself the
        // guarantee; if it ever does produce one, the finish must still reject.
        match attacker_auth.do_authentication(rp_origin(app), challenge) {
            Err(_) => { /* the allow-list already stopped it */ }
            Ok(assertion) => {
                let finish = app
                    .request_with_cookies(
                        Request::post("/user/webauthn/login/finish")
                            .header("content-type", "application/json")
                            .header("x-forwarded-for", fake_ip)
                            .body(Body::from(
                                serde_json::json!({ "credential": assertion }).to_string(),
                            ))
                            .unwrap(),
                        &cookies,
                    )
                    .await;
                assert_eq!(
                    finish.status(),
                    StatusCode::UNAUTHORIZED,
                    "a foreign credential must never complete another account's ceremony"
                );
            }
        }
    });
}
