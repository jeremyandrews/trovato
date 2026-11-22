#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `POST /user/logout` against the real Redis session store.
//!
//! Logging out ends the session: the answer is a redirect to `/user/login`, the
//! session record is gone from Redis, and the old cookie no longer signs anyone
//! in. Before the fix the handler deleted the record and the session layer then
//! tried to save the same, still modified session with `SET .. XX`, which Redis
//! answers with nil for a missing key; the store reads that reply as a bool,
//! fails, and the layer turned a successful logout into a 500.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, username};

const PASSWORD: &str = "logout-test-pw-1";

/// The session id: the value of the `id` cookie, which is also the Redis key.
fn session_id_of(cookies: &str) -> String {
    cookies
        .split("; ")
        .find_map(|c| c.strip_prefix("id="))
        .expect("session cookie")
        .to_string()
}

async fn record_exists(app: &TestApp, session_id: &str) -> bool {
    let mut conn = app
        .state
        .redis()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    redis::AsyncCommands::exists(&mut conn, session_id)
        .await
        .unwrap()
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

#[test]
fn logout_redirects_to_login_and_removes_the_session_from_redis() {
    run_test(async {
        let app = shared_app().await;
        let name = username("logout");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let session_id = session_id_of(&cookies);
        assert!(
            record_exists(app, &session_id).await,
            "the session record is in Redis after login"
        );

        let token = logout_token(app, &cookies).await;
        let response = app
            .request_with_cookies(
                Request::post("/user/logout")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("_token={token}")))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "logout answers a redirect, not a server error"
        );
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/user/login")
        );
        assert!(
            !record_exists(app, &session_id).await,
            "the session record must be gone from Redis after logout"
        );

        // The old cookie signs nobody in.
        let after = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert!(
            after.status().is_redirection(),
            "the old cookie no longer reaches the profile (status {})",
            after.status()
        );
    });
}

/// Logout stays POST with a valid token: a wrong token ends nothing.
#[test]
fn logout_with_a_bad_token_keeps_the_session() {
    run_test(async {
        let app = shared_app().await;
        let name = username("logout-bad");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let session_id = session_id_of(&cookies);

        let response = app
            .request_with_cookies(
                Request::post("/user/logout")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("_token=not-a-token"))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_ne!(response.status(), StatusCode::SEE_OTHER);
        assert!(
            record_exists(app, &session_id).await,
            "a refused logout leaves the session alone"
        );
    });
}

/// A blocked account's session is ended on its next request. The same delete
/// as logout, followed by a page that writes to the session (the login form's
/// CSRF token), so the same failed save once turned it into a 500.
#[test]
fn a_blocked_users_next_page_ends_the_session_without_an_error() {
    run_test(async {
        let app = shared_app().await;
        let name = username("logout-blocked");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let session_id = session_id_of(&cookies);
        let user_id = common::user_id_of(app, &name).await;
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
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the blocked user is shown the login form, not a server error"
        );
        assert!(
            !record_exists(app, &session_id).await,
            "the blocked user's session record must be gone"
        );
    });
}

/// A session revoked from another device (gone from the user's session index)
/// is ended on its next request, which renders a page that writes to the
/// session; the same failed save once made that a 500.
#[test]
fn a_revoked_sessions_next_page_ends_it_without_an_error() {
    run_test(async {
        let app = shared_app().await;
        let name = username("logout-revoked");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        // One request so the tracking middleware indexes the session.
        let _ = logout_token(app, &cookies).await;
        let session_id = session_id_of(&cookies);
        let user_id = common::user_id_of(app, &name).await;
        let mut conn = app
            .state
            .redis()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let _: () = redis::AsyncCommands::del(&mut conn, format!("user_sessions:{user_id}"))
            .await
            .unwrap();

        let response = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert!(
            !response.status().is_server_error(),
            "a revoked session's next request is not a server error (status {})",
            response.status()
        );
        assert!(
            !record_exists(app, &session_id).await,
            "the revoked session's record must be gone"
        );
    });
}
