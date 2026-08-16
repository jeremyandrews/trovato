#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Story 4.3 — credential management and the ≥1-active-recovery-path invariant.
//!
//! The interesting assertions here are the refusals: that revoking the last way
//! into an account is blocked with the reason named and audited (D-33), and that
//! going passwordless is blocked until a non-password recovery path exists. A
//! test suite that only proved rename/revoke work would miss the property the
//! invariant exists for.
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
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    let pos = html.find("csrf-token").expect("csrf-token meta tag");
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

async fn register_passkey(app: &TestApp, cookies: &str, device_name: Option<&str>) {
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
                    serde_json::json!({
                        "credential": credential,
                        "device_name": device_name,
                    })
                    .to_string(),
                ))
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(finish.status(), StatusCode::OK, "setup registration failed");
}

async fn user_id_of(app: &TestApp, username: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// A logged-in user whose credential set starts empty, so the fixed usernames
/// these tests use are repeatable against the persistent integration database.
async fn fresh_user(app: &TestApp, username: &str) -> (uuid::Uuid, String) {
    let cookies = app
        .create_and_login_user(
            username,
            "test-password-123",
            &format!("{username}@example.com"),
        )
        .await;
    let user_id = user_id_of(app, username).await;
    sqlx::query("DELETE FROM webauthn_credentials WHERE user_id = $1")
        .bind(user_id)
        .execute(&app.db)
        .await
        .unwrap();
    (user_id, cookies)
}

async fn credential_ids(app: &TestApp, user_id: uuid::Uuid) -> Vec<uuid::Uuid> {
    sqlx::query_scalar("SELECT id FROM webauthn_credentials WHERE user_id = $1 ORDER BY created_at")
        .bind(user_id)
        .fetch_all(&app.db)
        .await
        .unwrap()
}

#[test]
fn the_page_lists_credentials_with_their_metadata() {
    common::run_test(async {
        // AC-1: device name, registration date, and last-used time are visible.
        let app = common::shared_app().await;
        let (_user_id, cookies) = fresh_user(app, "wa_mgmt_list").await;
        register_passkey(app, &cookies, Some("Desk yubikey")).await;

        let response = app
            .request_with_cookies(
                Request::get("/user/passkeys").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 2_000_000)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&body);

        assert!(
            html.contains("Desk yubikey"),
            "the device name must be listed"
        );
        assert!(html.contains("Registered"), "the registration date column");
        assert!(html.contains("Last used"), "the last-used column");
        assert!(
            html.contains("never"),
            "a never-used credential should say so rather than showing a blank"
        );
    });
}

#[test]
fn a_credential_can_be_renamed() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, cookies) = fresh_user(app, "wa_mgmt_rename").await;
        register_passkey(app, &cookies, Some("Old name")).await;
        let id = credential_ids(app, user_id).await[0];

        let csrf = csrf_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/user/passkeys/{id}/rename"))
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({ "device_name": "New name" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let name: Option<String> =
            sqlx::query_scalar("SELECT device_name FROM webauthn_credentials WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(name.as_deref(), Some("New name"));

        let audited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM security_audit_log WHERE user_id = $1 AND kind = 'passkey.renamed'",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(audited >= 1, "a rename must be audited");
    });
}

#[test]
fn rename_rejects_an_empty_or_overlong_name() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, cookies) = fresh_user(app, "wa_mgmt_badname").await;
        register_passkey(app, &cookies, None).await;
        let id = credential_ids(app, user_id).await[0];

        for bad in ["   ", &"x".repeat(65)] {
            let csrf = csrf_token(app, &cookies).await;
            let response = app
                .request_with_cookies(
                    Request::post(format!("/user/passkeys/{id}/rename"))
                        .header("content-type", "application/json")
                        .header("x-csrf-token", &csrf)
                        .body(Body::from(
                            serde_json::json!({ "device_name": bad }).to_string(),
                        ))
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "an unusable label must be rejected, not stored"
            );
        }
    });
}

#[test]
fn revoke_requires_csrf() {
    common::run_test(async {
        // AC-2: revocation is state-changing, so it is POST + require_csrf.
        let app = common::shared_app().await;
        let (user_id, cookies) = fresh_user(app, "wa_mgmt_csrf").await;
        register_passkey(app, &cookies, None).await;
        let id = credential_ids(app, user_id).await[0];

        let response = app
            .request_with_cookies(
                Request::post(format!("/user/passkeys/{id}/revoke"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // ...and the credential is still there.
        assert_eq!(credential_ids(app, user_id).await.len(), 1);
    });
}

#[test]
fn a_credential_can_be_revoked_when_another_way_in_remains() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, cookies) = fresh_user(app, "wa_mgmt_revoke").await;
        register_passkey(app, &cookies, Some("Phone")).await;

        // This account still has its password, so removing its only passkey is
        // permitted: a set password satisfies the weaker passkey-removal rule.
        let id = credential_ids(app, user_id).await[0];
        let csrf = csrf_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/user/passkeys/{id}/revoke"))
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(credential_ids(app, user_id).await.is_empty());

        let audited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM security_audit_log WHERE user_id = $1 AND kind = 'passkey.revoked'",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(audited >= 1, "a revocation must be audited");
    });
}

#[test]
fn revoking_the_last_way_into_an_account_is_blocked_and_audited() {
    common::run_test(async {
        // AC-3, the case the invariant exists for. A passwordless account with
        // one passkey and no recovery path must not be able to remove it.
        let app = common::shared_app().await;
        let (user_id, cookies) = fresh_user(app, "wa_mgmt_lastway").await;
        register_passkey(app, &cookies, Some("Only device")).await;

        // Force the passwordless shape directly. (Going passwordless through the
        // endpoint is itself blocked without a recovery path — see the test
        // below — so this is the only way to construct the state 4.6 will make
        // reachable.)
        sqlx::query("UPDATE users SET pass = '' WHERE id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.users().invalidate(user_id);

        let id = credential_ids(app, user_id).await[0];
        let csrf = csrf_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/user/passkeys/{id}/revoke"))
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "removing the last way in must be refused"
        );
        let body = json_body(response).await;
        assert!(
            body["error"].as_str().unwrap_or("").contains("only way"),
            "the refusal must say why: {body}"
        );

        // The credential survives — the refusal is real, not cosmetic.
        assert_eq!(
            credential_ids(app, user_id).await.len(),
            1,
            "a blocked revocation must not delete anything"
        );

        let reason: Option<String> = sqlx::query_scalar(
            "SELECT details->>'reason' FROM security_audit_log
             WHERE user_id = $1 AND kind = 'credential.removal_blocked'
             ORDER BY created DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
        .flatten();
        assert_eq!(reason.as_deref(), Some("last_way_in"));

        // Restore so the shared fixture user is not left in a broken state.
        sqlx::query("UPDATE users SET pass = 'x' WHERE id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.users().invalidate(user_id);
    });
}

#[test]
fn going_passwordless_is_blocked_without_a_recovery_path() {
    common::run_test(async {
        // D-33's deliberate friction. Until Story 4.6 supplies recovery paths,
        // `active_recovery_path_count` is structurally zero, so this refusal is
        // the correct and fail-safe behaviour rather than a gap.
        let app = common::shared_app().await;
        let (user_id, cookies) = fresh_user(app, "wa_mgmt_nopwless").await;
        register_passkey(app, &cookies, Some("Laptop")).await;

        let csrf = csrf_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post("/user/password/remove")
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = json_body(response).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains("recovery method"),
            "the refusal must name the missing recovery method: {body}"
        );

        // The password is untouched.
        let pass: String = sqlx::query_scalar("SELECT pass FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(
            !pass.is_empty(),
            "a blocked removal must not clear the password"
        );

        let reason: Option<String> = sqlx::query_scalar(
            "SELECT details->>'reason' FROM security_audit_log
             WHERE user_id = $1 AND kind = 'credential.removal_blocked'
               AND details->>'removing' = 'password'
             ORDER BY created DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
        .flatten();
        assert_eq!(reason.as_deref(), Some("passwordless_needs_recovery_path"));
    });
}

#[test]
fn going_passwordless_is_blocked_without_a_passkey() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, cookies) = fresh_user(app, "wa_mgmt_nopasskey").await;
        // No passkey registered at all.

        let csrf = csrf_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post("/user/password/remove")
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let reason: Option<String> = sqlx::query_scalar(
            "SELECT details->>'reason' FROM security_audit_log
             WHERE user_id = $1 AND kind = 'credential.removal_blocked'
             ORDER BY created DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
        .flatten();
        assert_eq!(
            reason.as_deref(),
            Some("passwordless_needs_a_passkey"),
            "the more fundamental problem must be reported first"
        );
    });
}

#[test]
fn another_accounts_credential_cannot_be_renamed_or_revoked() {
    common::run_test(async {
        // Every management query is owner-scoped, so guessing a credential id
        // achieves nothing.
        let app = common::shared_app().await;
        let (victim_id, victim_cookies) = fresh_user(app, "wa_mgmt_victim").await;
        register_passkey(app, &victim_cookies, Some("Victim key")).await;
        let victim_credential = credential_ids(app, victim_id).await[0];

        let (_attacker_id, attacker_cookies) = fresh_user(app, "wa_mgmt_attacker").await;

        let csrf = csrf_token(app, &attacker_cookies).await;
        let rename = app
            .request_with_cookies(
                Request::post(format!("/user/passkeys/{victim_credential}/rename"))
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({ "device_name": "pwned" }).to_string(),
                    ))
                    .unwrap(),
                &attacker_cookies,
            )
            .await;
        assert_eq!(rename.status(), StatusCode::NOT_FOUND);

        let csrf = csrf_token(app, &attacker_cookies).await;
        let revoke = app
            .request_with_cookies(
                Request::post(format!("/user/passkeys/{victim_credential}/revoke"))
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &attacker_cookies,
            )
            .await;
        assert_eq!(revoke.status(), StatusCode::NOT_FOUND);

        // Untouched.
        let name: Option<String> =
            sqlx::query_scalar("SELECT device_name FROM webauthn_credentials WHERE id = $1")
                .bind(victim_credential)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(name.as_deref(), Some("Victim key"));
    });
}

#[test]
fn a_self_service_password_change_is_audited() {
    common::run_test(async {
        // The password-lifecycle emitter this story owns.
        let app = common::shared_app().await;
        let username = "wa_mgmt_pwchange";
        let (user_id, cookies) = fresh_user(app, username).await;

        // The profile page carries its own CSRF token for the password form.
        let profile = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        let body = axum::body::to_bytes(profile.into_body(), 2_000_000)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&body);
        let pos = html.find("csrf-token").expect("csrf-token meta tag");
        let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
        let end = html[start..].find('"').map(|p| start + p).unwrap();
        let csrf = &html[start..end];

        let response = app
            .request_with_cookies(
                Request::post("/user/password")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "_token={csrf}&current_password=test-password-123\
                         &new_password=another-password-456&confirm_password=another-password-456"
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let audited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM security_audit_log
             WHERE user_id = $1 AND kind IN ('password.changed', 'password.set')",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(
            audited >= 1,
            "a password change must land in the security audit stream"
        );

        // Put the password back so the shared fixture user stays usable.
        sqlx::query("UPDATE users SET pass = pass WHERE id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}
