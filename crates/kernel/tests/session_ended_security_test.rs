#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `session.ended`: a session that ends without being revoked writes one
//! security event.
//!
//! Before this, signing out wrote nothing to the security audit stream. A
//! login wrote `auth.login_succeeded` and `session.created`, and revoking a
//! session wrote `session.revoked_by_user` or `session.revoked_by_admin`, but
//! `POST /user/logout`, and the sign-out of a blocked account's session on its
//! next request, left the session's lifecycle with a start and no end.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, username};
use trovato_kernel::audit::hash_subject;
use uuid::Uuid;

const PASSWORD: &str = "session-ended-pw-1";

/// The session id: the value of the `id` cookie, which is also the Redis key.
fn session_id_of(cookies: &str) -> String {
    cookies
        .split("; ")
        .find_map(|c| c.strip_prefix("id="))
        .expect("session cookie")
        .to_string()
}

/// The CSRF token of the logout form on a signed-in page.
async fn logout_token(app: &TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/user/profile").body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "signed in before logout");
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let form = html
        .find("action=\"/user/logout\"")
        .expect("logout form on the page");
    let marker = "name=\"_token\" value=\"";
    let start = html[form..]
        .find(marker)
        .map(|p| form + p + marker.len())
        .unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

async fn logout(app: &TestApp, cookies: &str, token: &str) -> StatusCode {
    app.request_with_cookies(
        Request::post("/user/logout")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("_token={token}")))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// `(subject_hash, details)` of every `session.ended` row for `user_id`.
async fn ended_events(app: &TestApp, user_id: Uuid) -> Vec<(Option<String>, serde_json::Value)> {
    sqlx::query_as(
        "SELECT subject_hash, details FROM security_audit_log \
         WHERE kind = 'session.ended' AND user_id = $1 ORDER BY created, id",
    )
    .bind(user_id)
    .fetch_all(&app.db)
    .await
    .unwrap()
}

/// Logging out writes exactly one `session.ended`, reason `"logout"`, about
/// the session that was signed out, hashed, never the raw id.
#[test]
fn logout_writes_one_session_ended_event() {
    run_test(async {
        let app = shared_app().await;
        let name = username("sessended");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let user_id = common::user_id_of(app, &name).await;
        let session_id = session_id_of(&cookies);

        let token = logout_token(app, &cookies).await;
        assert_eq!(logout(app, &cookies, &token).await, StatusCode::SEE_OTHER);

        let events = ended_events(app, user_id).await;
        assert_eq!(events.len(), 1, "exactly one session.ended: {events:?}");
        let (subject, details) = &events[0];
        assert_eq!(subject.as_deref(), Some(hash_subject(&session_id).as_str()));
        assert_ne!(subject.as_deref(), Some(session_id.as_str()));
        assert_eq!(details["reason"], "logout");
        assert!(
            details["device_id"].as_str().is_some(),
            "names the device signed out: {details}"
        );
    });
}

/// A logout refused for a bad CSRF token ends nothing, so records nothing.
#[test]
fn a_refused_logout_writes_no_session_ended_event() {
    run_test(async {
        let app = shared_app().await;
        let name = username("sessendedbad");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let user_id = common::user_id_of(app, &name).await;

        assert_ne!(
            logout(app, &cookies, "not-a-token").await,
            StatusCode::SEE_OTHER
        );
        assert!(
            ended_events(app, user_id).await.is_empty(),
            "a refused logout must not be recorded as a sign-out"
        );
    });
}

/// A blocked account's session, signed out on its next request, writes one
/// `session.ended` with reason `"blocked"`.
#[test]
fn a_blocked_accounts_ended_session_writes_one_session_ended_event() {
    run_test(async {
        let app = shared_app().await;
        let name = username("sessendedblk");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let user_id = common::user_id_of(app, &name).await;
        let session_id = session_id_of(&cookies);
        sqlx::query("UPDATE users SET status = 0 WHERE id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.users().invalidate(user_id);

        let response = app
            .request_with_cookies(
                Request::get("/user/login").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let events = ended_events(app, user_id).await;
        assert_eq!(events.len(), 1, "exactly one session.ended: {events:?}");
        let (subject, details) = &events[0];
        assert_eq!(subject.as_deref(), Some(hash_subject(&session_id).as_str()));
        assert_eq!(details["reason"], "blocked");
    });
}
