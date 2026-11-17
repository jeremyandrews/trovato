#![allow(clippy::unwrap_used, clippy::expect_used)]
//! API token authentication through the full production middleware stack.
//!
//! An API token is a credential a client sends on every request. These pin
//! what that means at the HTTP layer:
//!
//! - a token request reaches the token middleware even though the OAuth JWT
//!   middleware sees the `Authorization: Bearer` header first;
//! - a token request is stateless: no `Set-Cookie`, no server-side session
//!   record, no entry in the per-user session index;
//! - revoking a token takes effect on the very next request, not after a cache
//!   expires;
//! - a session cookie still wins over a token on the same request;
//! - a token is not ambient, so a token-authenticated write needs no CSRF token,
//!   while a cookie-authenticated one still does.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use trovato_kernel::models::api_token::ApiToken;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";

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

/// A fresh user whose tokens are all deleted when the test ends, including any
/// a test mints through the HTTP API rather than through [`mint`].
async fn fresh_user(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    app.create_test_user(&name, PASSWORD, &format!("{name}@example.com"))
        .await;
    let user_id = common::user_id_of(app, &name).await;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM api_tokens WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
    });
    (user_id, name)
}

/// Mint a token the way the token admin surface does, and delete it again when
/// the test ends.
async fn mint(app: &TestApp, user_id: Uuid, name: &str) -> (Uuid, String) {
    let (token, raw) = ApiToken::create(&app.db, user_id, name, None)
        .await
        .expect("mint api token");
    let pool = app.db.clone();
    let id = token.id;
    common::defer_cleanup(move || async move {
        let _ = ApiToken::delete(&pool, id, user_id).await;
    });
    (token.id, raw)
}

/// `GET /api/tokens` authenticated by a bearer token, from the token owner's
/// own test IP so the request lands in its own rate-limit bucket.
async fn list_with_token(app: &TestApp, raw: &str, ip: &str) -> axum::response::Response {
    app.request(
        Request::get("/api/tokens")
            .header(header::AUTHORIZATION, format!("Bearer {raw}"))
            .header("x-forwarded-for", ip)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

fn token_names(body: &serde_json::Value) -> Vec<String> {
    body.as_array()
        .unwrap_or_else(|| panic!("expected a token list, got {body}"))
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect()
}

/// The session ids any `Set-Cookie` on `response` hands out.
fn issued_session_ids(response: &axum::response::Response) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|c| c.split(';').next())
        .filter_map(|kv| kv.split_once('='))
        .map(|(_, v)| v.to_string())
        .collect()
}

/// **A token request succeeds through the full production stack.**
///
/// The OAuth JWT middleware runs before the API token middleware, and used to
/// answer every `Authorization: Bearer` value that was not a JWT with 401 — so
/// an API token never reached the middleware that knows how to check it.
#[test]
fn a_token_request_is_authenticated_through_the_production_stack() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, name) = fresh_user(app, "tok-ok").await;
        let (_id, raw) = mint(app, user_id, "tok-ok token").await;

        let response = list_with_token(app, &raw, &common::test_ip_for(&name)).await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(status, StatusCode::OK, "a valid API token: {body}");
        assert_eq!(token_names(&body), vec!["tok-ok token".to_string()]);
    });
}

/// **A token request leaves no session behind.**
///
/// No `Set-Cookie` (so no site-wide cookie that outlives the token), no session
/// record in Redis, and no entry in the per-user session index, on the first
/// request or on any later one.
#[test]
fn a_token_request_creates_no_session_cookie_or_index_entry() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, name) = fresh_user(app, "tok-stateless").await;
        app.state
            .session_registry()
            .revoke_all_except(user_id, None)
            .await
            .ok();
        let (_id, raw) = mint(app, user_id, "stateless").await;
        let ip = common::test_ip_for(&name);

        for attempt in 1..=2 {
            let response = list_with_token(app, &raw, &ip).await;
            let issued = issued_session_ids(&response);
            let mut conn = app
                .state
                .redis()
                .get_multiplexed_async_connection()
                .await
                .unwrap();
            for id in &issued {
                let exists: bool = redis::AsyncCommands::exists(&mut conn, id).await.unwrap();
                assert!(
                    !exists,
                    "request {attempt}: a token request created the Redis session record {id}"
                );
            }
            assert!(
                issued.is_empty(),
                "request {attempt}: a token request must not set a cookie, got {:?}",
                response.headers().get_all(header::SET_COOKIE)
            );
            assert_eq!(response.status(), StatusCode::OK);
        }

        let indexed = app.state.session_registry().list(user_id).await.unwrap();
        assert!(
            indexed.is_empty(),
            "a token request must not register a browser session: {indexed:?}"
        );
    });
}

/// **Revocation is immediate.**
///
/// The token is looked up once (as a request would), revoked over HTTP, and the
/// very next lookup and the very next request both refuse it. A process-local
/// cache used to keep serving it for up to a minute.
#[test]
fn a_revoked_token_is_refused_on_the_very_next_request() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, name) = fresh_user(app, "tok-revoke").await;
        let (token_id, raw) = mint(app, user_id, "to revoke").await;
        let ip = common::test_ip_for(&name);

        // A lookup before revocation, the way the middleware does one.
        assert!(
            ApiToken::find_by_token(&app.db, &raw)
                .await
                .unwrap()
                .is_some()
        );

        // Revoke it from a browser session, with a CSRF token.
        let cookies = app.login(&name, PASSWORD).await;
        let page = app
            .request_with_cookies(
                Request::get("/user/sessions").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let csrf = csrf_from(&html_of(page).await);
        let revoke = app
            .request_with_cookies(
                Request::delete(format!("/api/tokens/{token_id}"))
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(revoke.status(), StatusCode::NO_CONTENT);

        assert!(
            ApiToken::find_by_token(&app.db, &raw)
                .await
                .unwrap()
                .is_none(),
            "a revoked token must not be served from a cache"
        );
        let after = list_with_token(app, &raw, &ip).await;
        assert_eq!(
            after.status(),
            StatusCode::UNAUTHORIZED,
            "the request right after revocation must be refused"
        );
    });
}

/// **A session cookie wins over a token.**
///
/// A request carrying both acts as the cookie's user; the token is ignored.
#[test]
fn a_cookie_and_a_token_together_act_as_the_cookie_user() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (cookie_user, cookie_name) = fresh_user(app, "tok-cookie").await;
        let (token_user, _) = fresh_user(app, "tok-other").await;
        mint(app, cookie_user, "cookie user's token").await;
        let (_id, raw) = mint(app, token_user, "token user's token").await;

        let cookies = app.login(&cookie_name, PASSWORD).await;
        let response = app
            .request_with_cookies(
                Request::get("/api/tokens")
                    .header(header::AUTHORIZATION, format!("Bearer {raw}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            token_names(&body),
            vec!["cookie user's token".to_string()],
            "the cookie's user, not the token's"
        );
    });
}

/// **A token-authenticated write needs no CSRF token; a cookie one still does.**
///
/// `POST /api/tokens` is a JSON write that checks CSRF. A token is not attached
/// to a cross-site request by any browser, so there is nothing to forge; a
/// cookie is, so the same write from a cookie alone is still refused.
#[test]
fn a_token_write_needs_no_csrf_but_a_cookie_write_still_does() {
    common::run_test(async {
        let app = common::shared_app().await;
        let (user_id, name) = fresh_user(app, "tok-csrf").await;
        let (_id, raw) = mint(app, user_id, "writer").await;
        let ip = common::test_ip_for(&name);

        let by_token = app
            .request(
                Request::post("/api/tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {raw}"))
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(r#"{"name":"made by a token"}"#))
                    .unwrap(),
            )
            .await;
        let status = by_token.status();
        let body = json_body(by_token).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "a token-authenticated write must not need CSRF: {body}"
        );

        let cookies = app.login(&name, PASSWORD).await;
        let by_cookie = app
            .request_with_cookies(
                Request::post("/api/tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"made by a cookie"}"#))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            by_cookie.status(),
            StatusCode::FORBIDDEN,
            "a cookie-authenticated write still needs a CSRF token"
        );

        let names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM api_tokens WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert!(names.contains(&"made by a token".to_string()));
        assert!(!names.contains(&"made by a cookie".to_string()));
    });
}
