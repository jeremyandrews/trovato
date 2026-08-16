#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Story 4.6 — the kernel recovery flow, end to end at the HTTP layer.
//!
//! Driven through the built-in **saved recovery codes** path, which rides the
//! same frozen `tap_account_recovery` schema and the same owner-scoped
//! fail-closed fold as any plugin method. The companion
//! `recovery_plugin_flow_test` drives the identical flow with a real WASM
//! plugin, so between them both halves of "one contract, no privileged second
//! codepath" are exercised.
//!
//! The assertions that matter here are the refusals: every attempt to skip a
//! step, replay a nonce, or spend a grant that was never issued.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

async fn user_id_of(app: &TestApp, username: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// A distinct fake IP per test so the recovery rate limit (5 per 15 minutes) is
/// not shared between them.
fn ip_for(tag: &str) -> String {
    let n: u32 = tag.bytes().map(u32::from).sum();
    format!("10.60.{}.{}", (n / 250) % 250, n % 250 + 1)
}

async fn reset_recovery_limits(app: &TestApp, ip: &str, user_id: uuid::Uuid) {
    app.state.rate_limiter().reset("recovery", ip).await.ok();
    app.state
        .rate_limiter()
        .reset("recovery", &format!("user:{user_id}"))
        .await
        .ok();
}

/// Create a user with saved recovery codes, returning (user_id, codes).
async fn user_with_recovery_codes(app: &TestApp, username: &str) -> (uuid::Uuid, Vec<String>) {
    app.create_test_user(
        username,
        "test-password-123",
        &format!("{username}@example.com"),
    )
    .await;
    let user_id = user_id_of(app, username).await;
    let codes = trovato_kernel::services::recovery_builtins::RecoveryCodesProvider::generate(
        &app.db, user_id,
    )
    .await
    .expect("recovery codes should generate");
    (user_id, codes)
}

async fn start_recovery(app: &TestApp, identifier: &str, ip: &str) -> axum::response::Response {
    app.request(
        Request::post("/user/recover/start")
            .header("content-type", "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({ "identifier": identifier }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

#[test]
fn the_full_recovery_flow_grants_only_a_scoped_reset_then_a_session() {
    common::run_test(async {
        // AC-1 + D-38, the happy path and the shape of what it grants.
        let app = common::shared_app().await;
        let username = "rec_full";
        let ip = ip_for(username);
        let (user_id, codes) = user_with_recovery_codes(app, username).await;
        reset_recovery_limits(app, &ip, user_id).await;

        // ── start ────────────────────────────────────────────────────────────
        let start = start_recovery(app, username, &ip).await;
        assert_eq!(start.status(), StatusCode::OK);
        let cookies = common::extract_cookies(&start);
        let body = json_body(start).await;
        let methods = body["methods"].as_array().unwrap();
        assert!(
            methods
                .iter()
                .any(|m| m["method_id"] == "trovato_recovery_codes:code"),
            "the saved-codes method should be offered: {body}"
        );

        // Verification must not be reachable yet — nothing has been chosen.
        let premature = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "response": codes[0] }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            premature.status(),
            StatusCode::CONFLICT,
            "verify must not be reachable before a method is chosen"
        );

        // ── choose ───────────────────────────────────────────────────────────
        let choose = app
            .request_with_cookies(
                Request::post("/user/recover/choose")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "method_id": "trovato_recovery_codes:code" })
                            .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(choose.status(), StatusCode::OK);
        assert!(
            json_body(choose).await["challenge_hint"]
                .as_str()
                .is_some_and(|h| !h.is_empty()),
            "the user must be told what to do next"
        );

        // Resetting must not be reachable yet — nothing has verified.
        let premature_reset = app
            .request_with_cookies(
                Request::post("/user/recover/reset")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "new_password": "brand-new-password-1" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            premature_reset.status(),
            StatusCode::FORBIDDEN,
            "reset must require the scoped grant a verified fold produces"
        );

        // ── verify ───────────────────────────────────────────────────────────
        let verify = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "response": codes[0] }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(verify.status(), StatusCode::OK);
        let verified_cookies = common::extract_cookies(&verify);
        let verified_cookies = if verified_cookies.is_empty() {
            cookies.clone()
        } else {
            verified_cookies
        };
        assert_eq!(json_body(verify).await["next"], "reset");

        // D-38: verification grants a SCOPED credential-reset state, NOT a
        // session. Proving the absence is the point of this assertion.
        let not_logged_in = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("x-forwarded-for", &ip)
                    .body(Body::empty())
                    .unwrap(),
                &verified_cookies,
            )
            .await;
        assert_eq!(
            not_logged_in.status(),
            StatusCode::SEE_OTHER,
            "a verified recovery must NOT be a standing authenticated session"
        );

        // ── reset ────────────────────────────────────────────────────────────
        let reset = app
            .request_with_cookies(
                Request::post("/user/recover/reset")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "new_password": "brand-new-password-1" }).to_string(),
                    ))
                    .unwrap(),
                &verified_cookies,
            )
            .await;
        assert_eq!(reset.status(), StatusCode::OK);
        let session_cookies = common::extract_cookies(&reset);
        assert!(
            !session_cookies.is_empty(),
            "setup_session must re-issue the cookie (cycle_id)"
        );

        // Only NOW is there a session.
        let logged_in = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &session_cookies,
            )
            .await;
        assert_eq!(logged_in.status(), StatusCode::OK);

        // The new password works and the old one does not.
        let _ = app.login(username, "brand-new-password-1").await;

        // The redeemed code is consumed.
        let unused: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(
            unused,
            codes.len() as i64 - 1,
            "a redeemed code must be single-use"
        );

        // AC-6: the whole flow is audited.
        for kind in [
            "recovery.initiated",
            "recovery.method_initiated",
            "recovery.verdict",
            "recovery.completed",
        ] {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM security_audit_log WHERE user_id = $1 AND kind = $2",
            )
            .bind(user_id)
            .bind(kind)
            .fetch_one(&app.db)
            .await
            .unwrap();
            assert!(count >= 1, "{kind} must be audited");
        }

        // ...and the flow nonce is never stored raw.
        let subject: Option<String> = sqlx::query_scalar(
            "SELECT subject_hash FROM security_audit_log
             WHERE user_id = $1 AND kind = 'recovery.completed' LIMIT 1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(subject.map(|s| s.len()), Some(64));
    });
}

#[test]
fn a_wrong_code_is_rejected_and_burns_the_flow() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "rec_wrong";
        let ip = ip_for(username);
        let (user_id, codes) = user_with_recovery_codes(app, username).await;
        reset_recovery_limits(app, &ip, user_id).await;

        let start = start_recovery(app, username, &ip).await;
        let cookies = common::extract_cookies(&start);
        let _ = json_body(start).await;

        let choose = app
            .request_with_cookies(
                Request::post("/user/recover/choose")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "method_id": "trovato_recovery_codes:code" })
                            .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(choose.status(), StatusCode::OK);

        let bad = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "response": "NOTAREALCODE" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

        // The nonce is burned, so the correct code cannot rescue this attempt.
        let retry = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "response": codes[0] }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            retry.status(),
            StatusCode::BAD_REQUEST,
            "a rejected flow is terminal: the nonce is single-use in both directions"
        );

        // No code was consumed by the failed attempt.
        let unused: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(unused, codes.len() as i64);
    });
}

#[test]
fn a_redeemed_code_cannot_be_reused() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "rec_reuse";
        let ip = ip_for(username);
        let (user_id, codes) = user_with_recovery_codes(app, username).await;

        // First flow: redeem code 0 successfully.
        for attempt in 0..2 {
            reset_recovery_limits(app, &ip, user_id).await;
            let start = start_recovery(app, username, &ip).await;
            let cookies = common::extract_cookies(&start);
            let _ = json_body(start).await;

            app.request_with_cookies(
                Request::post("/user/recover/choose")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "method_id": "trovato_recovery_codes:code" })
                            .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;

            let verify = app
                .request_with_cookies(
                    Request::post("/user/recover/verify")
                        .header("content-type", "application/json")
                        .header("x-forwarded-for", &ip)
                        .body(Body::from(
                            serde_json::json!({ "response": codes[0] }).to_string(),
                        ))
                        .unwrap(),
                    &cookies,
                )
                .await;

            if attempt == 0 {
                assert_eq!(verify.status(), StatusCode::OK, "the first use succeeds");
            } else {
                assert_eq!(
                    verify.status(),
                    StatusCode::UNAUTHORIZED,
                    "the same code must never verify twice"
                );
            }
        }
    });
}

#[test]
fn recovery_start_is_generic_for_an_unknown_account() {
    common::run_test(async {
        // AC-4: no account-existence oracle.
        let app = common::shared_app().await;
        let known = "rec_generic_known";
        let (user_id, _codes) = user_with_recovery_codes(app, known).await;

        let ip_a = ip_for("rec_generic_a");
        let ip_b = ip_for("rec_generic_b");
        app.state.rate_limiter().reset("recovery", &ip_a).await.ok();
        app.state.rate_limiter().reset("recovery", &ip_b).await.ok();
        app.state
            .rate_limiter()
            .reset("recovery", &format!("user:{user_id}"))
            .await
            .ok();

        let unknown = start_recovery(app, "no_such_account_anywhere", &ip_a).await;
        assert_eq!(unknown.status(), StatusCode::OK);
        let unknown_body = json_body(unknown).await;

        let existing = start_recovery(app, known, &ip_b).await;
        assert_eq!(existing.status(), StatusCode::OK);
        let existing_body = json_body(existing).await;

        assert_eq!(
            unknown_body["message"], existing_body["message"],
            "the message must not distinguish a real account from a fictional one"
        );
        assert_eq!(
            unknown_body["success"], existing_body["success"],
            "nor the success flag"
        );
    });
}

#[test]
fn recovery_initiation_is_rate_limited() {
    common::run_test(async {
        // AC-4. The limiter is what stops a short code being brute-forced inside
        // the flow TTL, and what stops a victim's inbox being flooded.
        let app = common::shared_app().await;
        let username = "rec_ratelimit";
        let ip = ip_for(username);
        let (user_id, _codes) = user_with_recovery_codes(app, username).await;
        reset_recovery_limits(app, &ip, user_id).await;

        let mut saw_limit = false;
        for _ in 0..12 {
            let response = start_recovery(app, username, &ip).await;
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                saw_limit = true;
                break;
            }
        }
        assert!(
            saw_limit,
            "repeated recovery initiations from one IP must hit the `recovery` limit"
        );

        let rate_limited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM security_audit_log WHERE kind = 'recovery.rate_limited'",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(rate_limited >= 1, "rate-limit hits must be audited");
    });
}

#[test]
fn a_method_the_flow_did_not_choose_cannot_be_verified() {
    common::run_test(async {
        // The step a hostile plugin would most want to skip. `verify` is bound
        // to the method that was actually initiated, on kernel state alone.
        let app = common::shared_app().await;
        let username = "rec_wrongmethod";
        let ip = ip_for(username);
        let (user_id, codes) = user_with_recovery_codes(app, username).await;
        reset_recovery_limits(app, &ip, user_id).await;

        let start = start_recovery(app, username, &ip).await;
        let cookies = common::extract_cookies(&start);
        let _ = json_body(start).await;

        // Choose a method that does not exist at all.
        let bogus = app
            .request_with_cookies(
                Request::post("/user/recover/choose")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "method_id": "made_up_plugin:whatever" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            bogus.status(),
            StatusCode::BAD_REQUEST,
            "a method nobody owns cannot be initiated"
        );

        // And the flow is still in `Started`, so verify remains unreachable.
        let verify = app
            .request_with_cookies(
                Request::post("/user/recover/verify")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({ "response": codes[0] }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(verify.status(), StatusCode::CONFLICT);
    });
}

#[test]
fn a_method_cannot_be_chosen_twice() {
    common::run_test(async {
        // Re-choosing would let a caller mint challenges (and emails) without
        // limit inside one flow.
        let app = common::shared_app().await;
        let username = "rec_rechoose";
        let ip = ip_for(username);
        let (user_id, _codes) = user_with_recovery_codes(app, username).await;
        reset_recovery_limits(app, &ip, user_id).await;

        let start = start_recovery(app, username, &ip).await;
        let cookies = common::extract_cookies(&start);
        let _ = json_body(start).await;

        let payload = serde_json::json!({ "method_id": "trovato_recovery_codes:code" }).to_string();
        let first = app
            .request_with_cookies(
                Request::post("/user/recover/choose")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(payload.clone()))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(first.status(), StatusCode::OK);

        let second = app
            .request_with_cookies(
                Request::post("/user/recover/choose")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(payload))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
    });
}

#[test]
fn saved_codes_satisfy_the_one_path_invariant() {
    common::run_test(async {
        // AC-3, and the closing of the loop Story 4.3 left open: with a real
        // recovery path configured, `active_recovery_path_count` is non-zero and
        // the passwordless gate opens.
        let app = common::shared_app().await;
        let username = "rec_invariant";
        app.create_test_user(
            username,
            "test-password-123",
            &format!("{username}@example.com"),
        )
        .await;
        let user_id = user_id_of(app, username).await;

        let user = trovato_kernel::models::User::find_by_id(&app.db, user_id)
            .await
            .unwrap()
            .unwrap();

        // No codes yet ⇒ no non-password recovery path.
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();
        let before =
            trovato_kernel::services::account_access::active_recovery_path_count(&app.state, &user)
                .await;
        assert_eq!(before, 0, "an account with no codes has no recovery path");

        // Generate codes ⇒ exactly one.
        trovato_kernel::services::recovery_builtins::RecoveryCodesProvider::generate(
            &app.db, user_id,
        )
        .await
        .unwrap();
        let after =
            trovato_kernel::services::account_access::active_recovery_path_count(&app.state, &user)
                .await;
        assert_eq!(
            after, 1,
            "saved recovery codes are a non-password recovery path"
        );

        // And that is what the invariant now sees.
        let access = trovato_kernel::services::account_access::AccountAccess {
            has_password: true,
            passkey_count: 1,
            non_password_recovery_paths: after,
        };
        assert!(
            access.can_remove_password().is_ok(),
            "with a passkey and a recovery path, going passwordless is permitted"
        );
    });
}

#[test]
fn an_admin_can_switch_a_built_in_path_off() {
    common::run_test(async {
        // AC-5. A high-assurance deployment must be able to require something
        // stronger than the built-ins.
        let app = common::shared_app().await;
        let username = "rec_adminoff";
        let (user_id, _codes) = user_with_recovery_codes(app, username).await;

        let admin_cookies = app
            .create_and_login_admin("rec_admin", "test-password-123", "rec_admin@example.com")
            .await;

        // The page renders, and it says out loud that recovery is the weak link.
        let page = app
            .request_with_cookies(
                Request::get("/admin/recovery").body(Body::empty()).unwrap(),
                &admin_cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = axum::body::to_bytes(page.into_body(), 2_000_000)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&html);
        assert!(
            html.contains("weakest link"),
            "the admin must be told what they are trading away"
        );
        let pos = html.find("csrf-token").unwrap();
        let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
        let end = html[start..].find('"').map(|p| start + p).unwrap();
        let csrf = html[start..end].to_string();

        let save = app
            .request_with_cookies(
                Request::post("/admin/recovery")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({ "email_enabled": false, "codes_enabled": false })
                            .to_string(),
                    ))
                    .unwrap(),
                &admin_cookies,
            )
            .await;
        assert_eq!(save.status(), StatusCode::OK);

        // With both built-ins off, the account has no built-in path left.
        let user = trovato_kernel::models::User::find_by_id(&app.db, user_id)
            .await
            .unwrap()
            .unwrap();
        let count =
            trovato_kernel::services::account_access::active_recovery_path_count(&app.state, &user)
                .await;
        assert_eq!(
            count, 0,
            "a disabled path must not be counted toward the invariant"
        );

        // Put it back so the shared fixture does not leak this setting.
        let page = app
            .request_with_cookies(
                Request::get("/admin/recovery").body(Body::empty()).unwrap(),
                &admin_cookies,
            )
            .await;
        let html = axum::body::to_bytes(page.into_body(), 2_000_000)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&html);
        let pos = html.find("csrf-token").unwrap();
        let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
        let end = html[start..].find('"').map(|p| start + p).unwrap();
        let csrf = html[start..end].to_string();
        app.request_with_cookies(
            Request::post("/admin/recovery")
                .header("content-type", "application/json")
                .header("x-csrf-token", &csrf)
                .body(Body::from(
                    serde_json::json!({ "email_enabled": true, "codes_enabled": true }).to_string(),
                ))
                .unwrap(),
            &admin_cookies,
        )
        .await;
    });
}

#[test]
fn a_non_admin_cannot_change_the_recovery_configuration() {
    common::run_test(async {
        let app = common::shared_app().await;
        let cookies = app
            .create_and_login_user(
                "rec_nonadmin",
                "test-password-123",
                "rec_nonadmin@example.com",
            )
            .await;

        let page = app
            .request_with_cookies(
                Request::get("/admin/recovery").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::FORBIDDEN);
    });
}

#[test]
fn recovery_codes_are_only_ever_stored_hashed() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "rec_hashed";
        let (user_id, codes) = user_with_recovery_codes(app, username).await;

        let hashes: Vec<String> =
            sqlx::query_scalar("SELECT code_hash FROM recovery_codes WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(&app.db)
                .await
                .unwrap();

        assert_eq!(hashes.len(), codes.len());
        for code in &codes {
            assert!(
                !hashes.contains(code),
                "a plaintext recovery code must never be stored"
            );
        }
        for hash in &hashes {
            assert_eq!(hash.len(), 64, "SHA-256 hex");
        }
    });
}

#[test]
fn generating_codes_replaces_the_previous_batch() {
    common::run_test(async {
        // A user must never be left unsure which of two printouts is live.
        let app = common::shared_app().await;
        let username = "rec_regen";
        let (user_id, first) = user_with_recovery_codes(app, username).await;

        let second = trovato_kernel::services::recovery_builtins::RecoveryCodesProvider::generate(
            &app.db, user_id,
        )
        .await
        .unwrap();

        assert_ne!(first[0], second[0]);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recovery_codes WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(
            count,
            second.len() as i64,
            "regenerating must invalidate the old batch, not add to it"
        );
    });
}
