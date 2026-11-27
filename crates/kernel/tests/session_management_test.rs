#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Story 4.4 — multi-device session management, end to end at the HTTP layer.
//!
//! The load-bearing assertion is invariant #2: a revoked session's **next
//! request fails**. Everything else (listing, naming, admin oversight, the audit
//! trail) is in service of that. These drive real logins through the real
//! middleware stack, so the index is maintained exactly as it is in production.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;

const UA_CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const UA_FIREFOX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0";

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

async fn html_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn csrf_from(html: &str) -> String {
    let pos = html.find("csrf-token").expect("csrf-token meta tag");
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

/// Log in with a specific User-Agent and drive one extra request so the
/// tracking middleware indexes the session, returning the live cookies.
async fn login_with_agent(app: &TestApp, username: &str, password: &str, ua: &str) -> String {
    app.state.lockout().clear_all(username).await.ok();
    let fake_ip = format!("10.70.{}.{}", username.len(), ua.len() % 250 + 1);
    app.state.rate_limiter().reset("login", &fake_ip).await.ok();

    let response = app
        .request(
            Request::post("/user/login/json")
                .header("content-type", "application/json")
                .header("x-forwarded-for", &fake_ip)
                .header("user-agent", ua)
                .body(Body::from(
                    serde_json::json!({ "username": username, "password": password }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "login failed");
    let cookies = common::extract_cookies(&response);

    // The index write happens after the handler on a request that already has
    // the authenticated session, so one follow-up request registers it.
    let follow = app
        .request_with_cookies(
            Request::get("/user/sessions")
                .header("user-agent", ua)
                .header("x-forwarded-for", &fake_ip)
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(follow.status(), StatusCode::OK);
    cookies
}

async fn sessions_page(app: &TestApp, cookies: &str, ua: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/user/sessions")
                .header("user-agent", ua)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    html_of(response).await
}

async fn user_id_of(app: &TestApp, username: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// A user whose session index starts empty, so fixed usernames are repeatable.
async fn fresh_user(app: &TestApp, username: &str) -> uuid::Uuid {
    app.create_test_user(
        username,
        "test-password-123",
        &format!("{username}@example.com"),
    )
    .await;
    let user_id = user_id_of(app, username).await;
    app.state
        .session_registry()
        .revoke_all_except(user_id, None)
        .await
        .ok();
    user_id
}

#[test]
fn a_login_appears_in_the_session_index_with_device_metadata() {
    common::run_test(async {
        // AC-1: device/browser label, IP, creation and last-activity times.
        let app = common::shared_app().await;
        let username = "sess_index";
        let user_id = fresh_user(app, username).await;

        let cookies = login_with_agent(app, username, "test-password-123", UA_CHROME).await;
        let html = sessions_page(app, &cookies, UA_CHROME).await;

        assert!(
            html.contains("Chrome on macOS"),
            "the User-Agent-derived label must appear: {}",
            &html[..html.len().min(400)]
        );
        assert!(
            html.contains("this device"),
            "the current session is marked"
        );

        let entries = app.state.session_registry().list(user_id).await.unwrap();
        assert_eq!(entries.len(), 1, "exactly one session should be indexed");
        let entry = &entries[0];
        assert_eq!(entry.device_name, "Chrome on macOS");
        assert!(!entry.ip.is_empty(), "the vetted client IP is recorded");
        assert!(entry.created_at > 0);
        assert!(entry.last_seen >= entry.created_at);
    });
}

#[test]
fn two_devices_produce_two_entries() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "sess_twodev";
        let user_id = fresh_user(app, username).await;

        let _chrome = login_with_agent(app, username, "test-password-123", UA_CHROME).await;
        let firefox = login_with_agent(app, username, "test-password-123", UA_FIREFOX).await;

        let entries = app.state.session_registry().list(user_id).await.unwrap();
        assert_eq!(entries.len(), 2, "one entry per device");

        let html = sessions_page(app, &firefox, UA_FIREFOX).await;
        assert!(html.contains("Chrome on macOS"));
        assert!(html.contains("Firefox on Linux"));
    });
}

#[test]
fn a_revoked_sessions_next_request_fails() {
    common::run_test(async {
        // Invariant #2. This is the whole point of the feature.
        let app = common::shared_app().await;
        let username = "sess_revoke";
        let user_id = fresh_user(app, username).await;

        let victim = login_with_agent(app, username, "test-password-123", UA_CHROME).await;
        let controller = login_with_agent(app, username, "test-password-123", UA_FIREFOX).await;

        // Identify the Chrome device from the controlling session.
        let entries = app.state.session_registry().list(user_id).await.unwrap();
        let target = entries
            .iter()
            .find(|e| e.device_name == "Chrome on macOS")
            .expect("the Chrome session should be indexed");

        // The victim session works right now.
        let before = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &victim,
            )
            .await;
        assert_eq!(before.status(), StatusCode::OK);

        let html = sessions_page(app, &controller, UA_FIREFOX).await;
        let csrf = csrf_from(&html);
        let revoke = app
            .request_with_cookies(
                Request::post(format!("/user/sessions/{}/revoke", target.device_id))
                    .header("x-csrf-token", &csrf)
                    .header("user-agent", UA_FIREFOX)
                    .body(Body::empty())
                    .unwrap(),
                &controller,
            )
            .await;
        assert_eq!(revoke.status(), StatusCode::OK);

        // ...and now it does not.
        let after = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &victim,
            )
            .await;
        assert_eq!(
            after.status(),
            StatusCode::UNAUTHORIZED,
            "a revoked session must not be able to reach a login-gated page"
        );

        // The controlling session is untouched.
        let controller_still_ok = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_FIREFOX)
                    .body(Body::empty())
                    .unwrap(),
                &controller,
            )
            .await;
        assert_eq!(controller_still_ok.status(), StatusCode::OK);

        // AC-5: the revocation is durably audited with a HASHED session id.
        let row: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT subject_hash, details->>'device_name' FROM security_audit_log
             WHERE user_id = $1 AND kind = 'session.revoked_by_user'
             ORDER BY created DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        let subject = row.0.expect("a hashed session id must be recorded");
        assert_eq!(subject.len(), 64, "SHA-256 hex, never the raw session id");
        assert_ne!(
            subject, target.session_id,
            "the raw id must never be stored"
        );
        assert_eq!(row.1.as_deref(), Some("Chrome on macOS"));
    });
}

#[test]
fn revoke_others_keeps_the_calling_session() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "sess_others";
        let user_id = fresh_user(app, username).await;

        let a = login_with_agent(app, username, "test-password-123", UA_CHROME).await;
        let keeper = login_with_agent(app, username, "test-password-123", UA_FIREFOX).await;
        assert_eq!(
            app.state
                .session_registry()
                .list(user_id)
                .await
                .unwrap()
                .len(),
            2
        );

        let html = sessions_page(app, &keeper, UA_FIREFOX).await;
        let csrf = csrf_from(&html);
        let response = app
            .request_with_cookies(
                Request::post("/user/sessions/revoke-others")
                    .header("x-csrf-token", &csrf)
                    .header("user-agent", UA_FIREFOX)
                    .body(Body::empty())
                    .unwrap(),
                &keeper,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["revoked"], 1);

        // The caller is still signed in — otherwise the action defeats itself.
        let keeper_ok = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_FIREFOX)
                    .body(Body::empty())
                    .unwrap(),
                &keeper,
            )
            .await;
        assert_eq!(keeper_ok.status(), StatusCode::OK);

        // The other device is out.
        let other = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &a,
            )
            .await;
        assert_eq!(other.status(), StatusCode::UNAUTHORIZED);
    });
}

#[test]
fn a_session_can_be_renamed() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "sess_rename";
        let user_id = fresh_user(app, username).await;
        let cookies = login_with_agent(app, username, "test-password-123", UA_CHROME).await;

        let entries = app.state.session_registry().list(user_id).await.unwrap();
        let device_id = entries[0].device_id;

        let html = sessions_page(app, &cookies, UA_CHROME).await;
        let csrf = csrf_from(&html);
        let response = app
            .request_with_cookies(
                Request::post(format!("/user/sessions/{device_id}/rename"))
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .header("user-agent", UA_CHROME)
                    .body(Body::from(
                        serde_json::json!({ "device_name": "Kitchen iMac" }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let entries = app.state.session_registry().list(user_id).await.unwrap();
        assert_eq!(entries[0].device_name, "Kitchen iMac");
    });
}

#[test]
fn revoking_requires_csrf() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "sess_csrf";
        let user_id = fresh_user(app, username).await;
        let cookies = login_with_agent(app, username, "test-password-123", UA_CHROME).await;
        let device_id = app.state.session_registry().list(user_id).await.unwrap()[0].device_id;

        let response = app
            .request_with_cookies(
                Request::post(format!("/user/sessions/{device_id}/revoke"))
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            app.state
                .session_registry()
                .list(user_id)
                .await
                .unwrap()
                .len(),
            1,
            "a CSRF-rejected revoke must not have removed anything"
        );
    });
}

#[test]
fn one_user_cannot_revoke_another_users_session() {
    common::run_test(async {
        let app = common::shared_app().await;
        let victim = "sess_victim";
        let attacker = "sess_attacker";
        let victim_id = fresh_user(app, victim).await;
        fresh_user(app, attacker).await;

        let _victim_cookies = login_with_agent(app, victim, "test-password-123", UA_CHROME).await;
        let attacker_cookies =
            login_with_agent(app, attacker, "test-password-123", UA_FIREFOX).await;

        let victim_device =
            app.state.session_registry().list(victim_id).await.unwrap()[0].device_id;

        let html = sessions_page(app, &attacker_cookies, UA_FIREFOX).await;
        let csrf = csrf_from(&html);
        let response = app
            .request_with_cookies(
                Request::post(format!("/user/sessions/{victim_device}/revoke"))
                    .header("x-csrf-token", &csrf)
                    .header("user-agent", UA_FIREFOX)
                    .body(Body::empty())
                    .unwrap(),
                &attacker_cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "revocation is scoped to the caller's own account"
        );
        assert_eq!(
            app.state
                .session_registry()
                .list(victim_id)
                .await
                .unwrap()
                .len(),
            1,
            "the victim's session must survive"
        );
    });
}

#[test]
fn an_admin_can_view_and_revoke_any_users_session() {
    common::run_test(async {
        // AC-3.
        let app = common::shared_app().await;
        let subject = "sess_admin_subject";
        let subject_id = fresh_user(app, subject).await;
        let subject_cookies = login_with_agent(app, subject, "test-password-123", UA_CHROME).await;

        let admin_cookies = app
            .create_and_login_admin("sess_admin", "test-password-123", "sess_admin@example.com")
            .await;

        let page = app
            .request_with_cookies(
                Request::get(format!("/admin/users/{subject_id}/sessions"))
                    .header("user-agent", UA_FIREFOX)
                    .body(Body::empty())
                    .unwrap(),
                &admin_cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = html_of(page).await;
        assert!(
            html.contains("Chrome on macOS"),
            "the admin sees the device"
        );
        assert!(html.contains(subject), "the page names the account");

        let device_id = app.state.session_registry().list(subject_id).await.unwrap()[0].device_id;
        let csrf = csrf_from(&html);

        let revoke = app
            .request_with_cookies(
                Request::post(format!(
                    "/admin/users/{subject_id}/sessions/{device_id}/revoke"
                ))
                .header("x-csrf-token", &csrf)
                .header("user-agent", UA_FIREFOX)
                .body(Body::empty())
                .unwrap(),
                &admin_cookies,
            )
            .await;
        assert_eq!(revoke.status(), StatusCode::OK);

        // The subject is signed out on their next request.
        let after = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &subject_cookies,
            )
            .await;
        assert_eq!(after.status(), StatusCode::UNAUTHORIZED);

        // Audited as an ADMIN revocation, recording both the subject and the actor.
        let row: (Option<uuid::Uuid>, Option<uuid::Uuid>) = sqlx::query_as(
            "SELECT user_id, actor_id FROM security_audit_log
             WHERE kind = 'session.revoked_by_admin' AND user_id = $1
             ORDER BY created DESC LIMIT 1",
        )
        .bind(subject_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(row.0, Some(subject_id), "the subject account is recorded");
        assert!(
            row.1.is_some_and(|a| a != subject_id),
            "the acting admin is recorded and is not the subject"
        );
    });
}

#[test]
fn a_non_admin_cannot_reach_the_admin_session_views() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = "sess_nonadmin";
        let user_id = fresh_user(app, username).await;
        let cookies = login_with_agent(app, username, "test-password-123", UA_CHROME).await;

        let page = app
            .request_with_cookies(
                Request::get(format!("/admin/users/{user_id}/sessions"))
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            page.status(),
            StatusCode::FORBIDDEN,
            "admin oversight must require the admin permission"
        );
    });
}

#[test]
fn the_session_index_survives_a_cycle_id() {
    common::run_test(async {
        // AC-1's migration clause. A password change cycles the session id; the
        // device must stay ONE row, updated in place, not become a phantom pair.
        let app = common::shared_app().await;
        let username = "sess_cycle";
        let user_id = fresh_user(app, username).await;
        let cookies = login_with_agent(app, username, "test-password-123", UA_CHROME).await;

        let before = app.state.session_registry().list(user_id).await.unwrap();
        assert_eq!(before.len(), 1);
        let device_id = before[0].device_id;
        let old_session_id = before[0].session_id.clone();

        // Change the password through the real self-service path, which calls
        // session.cycle_id().
        let profile = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = html_of(profile).await;
        let csrf = csrf_from(&html);

        let changed = app
            .request_with_cookies(
                Request::post("/user/password")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("user-agent", UA_CHROME)
                    .body(Body::from(format!(
                        "_token={csrf}&current_password=test-password-123\
                         &new_password=cycled-password-456&confirm_password=cycled-password-456"
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let cycled_cookies = common::extract_cookies(&changed);
        assert!(
            !cycled_cookies.is_empty(),
            "cycle_id must re-issue the cookie"
        );

        // One more request so the middleware observes the new session id.
        let follow = app
            .request_with_cookies(
                Request::get("/user/sessions")
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &cycled_cookies,
            )
            .await;
        assert_eq!(follow.status(), StatusCode::OK);

        let after = app.state.session_registry().list(user_id).await.unwrap();
        assert_eq!(
            after.len(),
            1,
            "the cycle must MIGRATE the entry, not orphan the old one and add a second"
        );
        assert_eq!(
            after[0].device_id, device_id,
            "the device identity is stable across the cycle"
        );
        assert_ne!(
            after[0].session_id, old_session_id,
            "the entry must now point at the new session id"
        );

        // And the cycle is audited.
        let cycled_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM security_audit_log
             WHERE user_id = $1 AND kind = 'session.id_cycled'",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(cycled_events >= 1, "a cycle_id must be audited");
    });
}

#[test]
fn last_seen_writes_are_throttled() {
    common::run_test(async {
        // AC-4: a burst of requests must not become a burst of Redis writes.
        let app = common::shared_app().await;
        let username = "sess_throttle";
        let user_id = fresh_user(app, username).await;
        let cookies = login_with_agent(app, username, "test-password-123", UA_CHROME).await;

        let first = app.state.session_registry().list(user_id).await.unwrap()[0].last_seen;

        for _ in 0..5 {
            let response = app
                .request_with_cookies(
                    Request::get("/user/sessions")
                        .header("user-agent", UA_CHROME)
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let after = app.state.session_registry().list(user_id).await.unwrap()[0].last_seen;
        assert_eq!(
            after, first,
            "requests inside the throttle window must not rewrite last_seen"
        );
    });
}

// ─── Sessions end when the account changes ──────────────────────────────────

const NEW_PASSWORD: &str = "a-brand-new-password-9";

/// `GET /user/profile` with `cookies`: 200 for a live session, 303 to the login
/// page for one that has ended.
async fn profile_status(app: &TestApp, cookies: &str, ua: &str) -> StatusCode {
    app.request_with_cookies(
        Request::get("/user/profile")
            .header("user-agent", ua)
            .body(Body::empty())
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// The `_token` value of the first form on `html`.
fn form_token_from(html: &str) -> String {
    let marker = r#"name="_token" value=""#;
    let start = html.find(marker).expect("a CSRF form field") + marker.len();
    let end = start + html[start..].find('"').unwrap();
    html[start..end].to_string()
}

/// The cookies a response re-issued, or the ones the request carried.
fn cookies_after(response: &axum::response::Response, before: &str) -> String {
    let issued = common::extract_cookies(response);
    if issued.is_empty() {
        before.to_string()
    } else {
        issued
    }
}

/// A logged-in superuser, unique per call.
async fn admin_cookies(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await
}

/// Save the admin user-edit form for `target`, optionally blocking it or
/// setting its password.
async fn admin_edit(
    app: &TestApp,
    admin: &str,
    target_id: uuid::Uuid,
    target_name: &str,
    active: bool,
    password: Option<&str>,
) -> StatusCode {
    let path = format!("/admin/people/{target_id}/edit");
    let page = app
        .request_with_cookies(Request::get(&path).body(Body::empty()).unwrap(), admin)
        .await;
    assert_eq!(page.status(), StatusCode::OK, "the edit form renders");
    let token = form_token_from(&html_of(page).await);
    let mut body = format!(
        "_token={}&_form_build_id=x&name={}&mail={}",
        urlencoding::encode(&token),
        urlencoding::encode(target_name),
        urlencoding::encode(&format!("{target_name}@example.com")),
    );
    if active {
        body.push_str("&status=1");
    }
    if let Some(password) = password {
        body.push_str(&format!("&password={}", urlencoding::encode(password)));
    }
    app.request_with_cookies(
        Request::post(&path)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        admin,
    )
    .await
    .status()
}

/// **A password change ends the account's other sessions and keeps this one.**
#[test]
fn a_password_change_ends_other_sessions_and_keeps_the_callers() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = common::username("sess-pwchange");
        let _user_id = fresh_user(app, &username).await;

        let here = login_with_agent(app, &username, "test-password-123", UA_CHROME).await;
        let elsewhere = login_with_agent(app, &username, "test-password-123", UA_FIREFOX).await;

        let profile = app
            .request_with_cookies(
                Request::get("/user/profile")
                    .header("user-agent", UA_CHROME)
                    .body(Body::empty())
                    .unwrap(),
                &here,
            )
            .await;
        assert_eq!(profile.status(), StatusCode::OK);
        let token = form_token_from(&html_of(profile).await);
        let change = app
            .request_with_cookies(
                Request::post("/user/password")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("user-agent", UA_CHROME)
                    .body(Body::from(format!(
                        "_token={}&current_password=test-password-123&new_password={NEW_PASSWORD}\
                         &confirm_password={NEW_PASSWORD}",
                        urlencoding::encode(&token)
                    )))
                    .unwrap(),
                &here,
            )
            .await;
        assert_eq!(change.status(), StatusCode::OK, "the change succeeds");
        let here = cookies_after(&change, &here);

        assert_eq!(
            profile_status(app, &elsewhere, UA_FIREFOX).await,
            StatusCode::UNAUTHORIZED,
            "a password change must end the account's other sessions"
        );
        assert_eq!(
            profile_status(app, &here, UA_CHROME).await,
            StatusCode::OK,
            "the session that changed the password stays signed in"
        );
    });
}

/// **A password reset through an emailed token ends every session.**
#[test]
fn a_password_reset_by_token_ends_every_session() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = common::username("sess-pwreset");
        let user_id = fresh_user(app, &username).await;

        let one = login_with_agent(app, &username, "test-password-123", UA_CHROME).await;
        let two = login_with_agent(app, &username, "test-password-123", UA_FIREFOX).await;

        let (_, token) =
            trovato_kernel::models::password_reset::PasswordResetToken::create(&app.db, user_id)
                .await
                .expect("mint a reset token");
        let reset = app
            .request(
                Request::post(format!("/user/password-reset/{token}"))
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", common::test_ip_for(&username))
                    .body(Body::from(
                        serde_json::json!({ "password": NEW_PASSWORD }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(reset.status(), StatusCode::OK, "the reset succeeds");

        for (cookies, ua) in [(&one, UA_CHROME), (&two, UA_FIREFOX)] {
            assert_eq!(
                profile_status(app, cookies, ua).await,
                StatusCode::UNAUTHORIZED,
                "a password reset must end every session of the account"
            );
        }
    });
}

/// **Recovering an account ends the sessions it had before.**
///
/// The new session the recovery establishes is the only one left.
#[test]
fn a_recovery_reset_ends_prior_sessions() {
    common::run_test(async {
        let app = common::shared_app().await;
        let username = common::username("sess-recover");
        let user_id = fresh_user(app, &username).await;
        let codes = trovato_kernel::services::recovery_builtins::RecoveryCodesProvider::generate(
            &app.db, user_id,
        )
        .await
        .expect("recovery codes");
        let prior = login_with_agent(app, &username, "test-password-123", UA_CHROME).await;

        let ip = common::test_ip_for(&format!("{username}-recovery"));
        app.state.rate_limiter().reset("recovery", &ip).await.ok();
        app.state
            .rate_limiter()
            .reset("recovery", &format!("user:{user_id}"))
            .await
            .ok();

        let mut flow = common::RecoveryVisit::open(app, &ip).await;
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
                serde_json::json!({ "response": codes[0] }).to_string(),
            )
            .await;
        assert_eq!(verify.status(), StatusCode::OK);
        let reset = flow
            .post(
                app,
                "/user/recover/reset",
                serde_json::json!({ "new_password": NEW_PASSWORD }).to_string(),
            )
            .await;
        assert_eq!(
            reset.status(),
            StatusCode::OK,
            "the recovery reset succeeds"
        );
        let recovered = flow.cookies.clone();

        assert_eq!(
            profile_status(app, &prior, UA_CHROME).await,
            StatusCode::UNAUTHORIZED,
            "a recovery must end the sessions the account had before it"
        );
        assert_eq!(
            profile_status(app, &recovered, UA_FIREFOX).await,
            StatusCode::OK,
            "the session the recovery established is signed in"
        );
    });
}

/// **Blocking an account ends its sessions and deletes its API tokens.**
///
/// A blocked user's next request was already refused by `require_login`; what
/// this pins is that nothing of theirs is left to come back with: no indexed
/// session, no session record, no token.
#[test]
fn blocking_a_user_ends_their_sessions_and_deletes_their_tokens() {
    common::run_test(async {
        let app = common::shared_app().await;
        let admin = admin_cookies(app, "sess-blocker").await;
        let username = common::username("sess-blocked");
        let user_id = fresh_user(app, &username).await;
        let _victim = login_with_agent(app, &username, "test-password-123", UA_CHROME).await;
        let (token, _raw) = trovato_kernel::models::api_token::ApiToken::create(
            &app.db,
            user_id,
            "blocked user's token",
            None,
        )
        .await
        .expect("mint a token");
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ =
                trovato_kernel::models::api_token::ApiToken::delete(&pool, token.id, user_id).await;
        });
        let indexed = app.state.session_registry().list(user_id).await.unwrap();
        assert_eq!(indexed.len(), 1, "the user's session is indexed");
        let session_id = indexed[0].session_id.clone();

        let status = admin_edit(app, &admin, user_id, &username, false, None).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "the block is saved");

        assert!(
            app.state
                .session_registry()
                .list(user_id)
                .await
                .unwrap()
                .is_empty(),
            "blocking must end every session of the account"
        );
        let mut conn = app
            .state
            .redis()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let record: bool = redis::AsyncCommands::exists(&mut conn, &session_id)
            .await
            .unwrap();
        assert!(!record, "the blocked user's session record must be gone");
        let tokens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_tokens WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(tokens, 0, "blocking must delete the account's API tokens");

        // And records which went: one `api_token.revoked` for the token,
        // naming the block as the reason and the administrator as the actor.
        let revoked: Vec<(Option<String>, Option<uuid::Uuid>, serde_json::Value)> = sqlx::query_as(
            "SELECT subject_hash, actor_id, details FROM security_audit_log \
                 WHERE kind = 'api_token.revoked' AND user_id = $1",
        )
        .bind(user_id)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(revoked.len(), 1, "one event per token: {revoked:?}");
        let (subject, actor, details) = &revoked[0];
        assert_eq!(
            subject.as_deref(),
            Some(trovato_kernel::audit::hash_subject(&token.id.to_string()).as_str())
        );
        assert!(
            actor.is_some_and(|a| a != user_id),
            "the administrator who blocked it: {actor:?}"
        );
        assert_eq!(details["reason"], "account_blocked");
    });
}

/// **An administrator setting another user's password ends that user's sessions.**
#[test]
fn an_admin_setting_a_users_password_ends_their_sessions() {
    common::run_test(async {
        let app = common::shared_app().await;
        let admin = admin_cookies(app, "sess-pwadmin").await;
        let username = common::username("sess-pwtarget");
        let user_id = fresh_user(app, &username).await;
        let victim = login_with_agent(app, &username, "test-password-123", UA_CHROME).await;

        let status = admin_edit(app, &admin, user_id, &username, true, Some(NEW_PASSWORD)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "the edit is saved");

        assert_eq!(
            profile_status(app, &victim, UA_CHROME).await,
            StatusCode::UNAUTHORIZED,
            "a password set by an administrator must end the user's sessions"
        );
        assert_eq!(
            profile_status(app, &admin, UA_FIREFOX).await,
            StatusCode::OK,
            "the administrator's own session is untouched"
        );
    });
}
