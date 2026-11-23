#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The API token page, `/user/tokens`.
//!
//! `/api/tokens` could create, list and revoke tokens, and nothing on the site
//! offered it: a person could not issue themselves a key through the interface.
//! The page lists the user's tokens, creates one through the existing endpoint
//! with the page's CSRF token (the raw value is shown once, in the browser, and
//! never rendered by the server), and revokes one the same way.
//!
//! What a browser does is driven here the way the page's script does it: the
//! page is fetched for its CSRF token, and the JSON endpoints are called with it
//! in `X-CSRF-Token` on the same cookie session.
//!
//! Requires Postgres and Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::api_token::ApiToken;
use uuid::Uuid;

const PASSWORD: &str = "test-password-123";

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

/// A fresh, signed-in user whose tokens are all deleted when the test ends.
async fn signed_in_user(app: &TestApp) -> (Uuid, String, String) {
    let name = common::username("b69tok");
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
    let cookies = app.login(&name, PASSWORD).await;
    let ip = common::test_ip_for(&name);
    (user_id, cookies, ip)
}

async fn get_page(app: &TestApp, path: &str, cookies: &str, ip: &str) -> (StatusCode, String) {
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
    (status, html_of(response).await)
}

/// The page lists the user's own tokens by name, with when each was created,
/// last used and expires, and never renders a token's secret.
#[test]
fn the_token_page_lists_the_users_tokens() {
    run_test(async {
        let app = shared_app().await;
        let (user_id, cookies, ip) = signed_in_user(app).await;
        let (_, first_raw) = ApiToken::create(&app.db, user_id, "b69 deploy key", None)
            .await
            .expect("create token");
        let (_, second_raw) = ApiToken::create(
            &app.db,
            user_id,
            "b69 reporting key",
            Some(chrono::Utc::now() + chrono::Duration::days(30)),
        )
        .await
        .expect("create token");

        let (status, html) = get_page(app, "/user/tokens", &cookies, &ip).await;
        assert_eq!(status, StatusCode::OK, "the token page must render");
        assert!(html.contains("b69 deploy key"), "the first token is listed");
        assert!(
            html.contains("b69 reporting key"),
            "the second token is listed"
        );
        assert!(
            !html.contains(&first_raw) && !html.contains(&second_raw),
            "the server never renders a token's secret"
        );
        for column in ["Created", "Last used", "Expires"] {
            assert!(html.contains(column), "the list shows {column}");
        }
    });
}

/// Someone else's token is not on my page.
#[test]
fn the_token_page_shows_only_the_users_own_tokens() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies, ip) = signed_in_user(app).await;
        let (other_id, _, _) = signed_in_user(app).await;
        let other_name = format!("b69 someone else {}", Uuid::now_v7().simple());
        ApiToken::create(&app.db, other_id, &other_name, None)
            .await
            .expect("create token");

        let (status, html) = get_page(app, "/user/tokens", &cookies, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !html.contains(&other_name),
            "another user's token must not be listed"
        );
    });
}

/// The page offers the create form, warns that the token is shown once, and
/// loads the script that does it (an inline script would not run under the
/// site's CSP).
#[test]
fn the_token_page_offers_a_create_form_and_says_the_token_is_shown_once() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies, ip) = signed_in_user(app).await;

        let (status, html) = get_page(app, "/user/tokens", &cookies, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("name=\"name\""),
            "the form asks for a token name"
        );
        assert!(
            html.contains("name=\"expires_in_days\""),
            "the form asks for an expiry in days"
        );
        assert!(
            html.contains("will not be shown again"),
            "the page warns that the token is shown once"
        );
        assert!(
            html.contains("src=\"/static/js/api-tokens.js\""),
            "the page loads its script from a file"
        );
    });
}

/// Create with the page's CSRF token, find the token listed (and its secret
/// not), then revoke it with a fresh page's CSRF token and find it gone.
#[test]
fn a_token_is_created_and_revoked_with_the_pages_csrf_token() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies, ip) = signed_in_user(app).await;
        let name = format!("b69 page key {}", Uuid::now_v7().simple());

        let (_, html) = get_page(app, "/user/tokens", &cookies, &ip).await;
        let csrf = csrf_from(&html);
        let response = app
            .request_with_cookies(
                Request::post("/api/tokens")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({ "name": name, "expires_in_days": 30 }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created: serde_json::Value = serde_json::from_str(&html_of(response).await).unwrap();
        let raw = created["token"].as_str().expect("raw token").to_string();
        let id = created["id"].as_str().expect("token id").to_string();

        let (_, html) = get_page(app, "/user/tokens", &cookies, &ip).await;
        assert!(html.contains(&name), "the new token is listed on reload");
        assert!(
            !html.contains(&raw),
            "the raw token is gone once the page is reloaded"
        );
        assert!(
            html.contains(&id),
            "the listed token carries its id for the revoke button"
        );

        let csrf = csrf_from(&html);
        let response = app
            .request_with_cookies(
                Request::delete(format!("/api/tokens/{id}"))
                    .header("x-forwarded-for", &ip)
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let (_, html) = get_page(app, "/user/tokens", &cookies, &ip).await;
        assert!(!html.contains(&name), "a revoked token is no longer listed");
    });
}

/// The page is the signed-in user's; anyone else is sent to sign in.
#[test]
fn the_token_page_requires_sign_in() {
    run_test(async {
        let app = shared_app().await;
        let response = app
            .request(
                Request::get("/user/tokens")
                    .header("x-forwarded-for", common::test_ip_for("b69-anon-tokens"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an anonymous visitor is refused as nobody signed in"
        );
    });
}

/// The account page links to the token page.
#[test]
fn the_profile_page_links_to_the_token_page() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies, ip) = signed_in_user(app).await;
        let (status, html) = get_page(app, "/user/profile", &cookies, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("href=\"/user/tokens\""),
            "the profile links to /user/tokens"
        );
    });
}
