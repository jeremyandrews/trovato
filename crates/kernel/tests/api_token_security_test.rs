#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `api_token.created` and `api_token.revoked`: issuing or revoking an API
//! token, a credential that signs its holder in, writes a security event.
//!
//! Before this, neither did. `POST /api/tokens` and `DELETE /api/tokens/{id}`
//! (what the `/user/tokens` page calls) wrote the token row and nothing else,
//! and an administrator blocking an account deleted every token it held with
//! no record of which went. The block's path is covered in
//! `session_management_test.rs`, beside the test that the block deletes them.
//!
//! Requires Postgres and Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::audit::hash_subject;
use uuid::Uuid;

const PASSWORD: &str = "test-password-123";

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

/// The CSRF token of a fresh `/user/tokens` page; each is good for one write.
async fn page_csrf(app: &TestApp, cookies: &str, ip: &str) -> String {
    let page = app
        .request_with_cookies(
            Request::get("/user/tokens")
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(page.status(), StatusCode::OK);
    let html = body_of(page).await;
    let pos = html.find("csrf-token").expect("csrf-token meta tag");
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

/// A fresh, signed-in user whose tokens are all deleted when the test ends,
/// and the CSRF token of its `/user/tokens` page.
async fn signed_in_user(app: &TestApp, prefix: &str) -> (Uuid, String, String, String) {
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
    let cookies = app.login(&name, PASSWORD).await;
    let ip = common::test_ip_for(&name);
    let csrf = page_csrf(app, &cookies, &ip).await;
    (user_id, cookies, ip, csrf)
}

async fn create(
    app: &TestApp,
    cookies: &str,
    ip: &str,
    csrf: &str,
    name: &str,
) -> axum::response::Response {
    app.request_with_cookies(
        Request::post("/api/tokens")
            .header("content-type", "application/json")
            .header("x-forwarded-for", ip)
            .header("x-csrf-token", csrf)
            .body(Body::from(
                serde_json::json!({ "name": name, "expires_in_days": 30 }).to_string(),
            ))
            .unwrap(),
        cookies,
    )
    .await
}

async fn revoke(app: &TestApp, cookies: &str, ip: &str, csrf: &str, id: &str) -> StatusCode {
    app.request_with_cookies(
        Request::delete(format!("/api/tokens/{id}"))
            .header("x-forwarded-for", ip)
            .header("x-csrf-token", csrf)
            .body(Body::empty())
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// `(subject_hash, details)` of every row of `kind` for `user_id`.
async fn events(
    app: &TestApp,
    kind: &str,
    user_id: Uuid,
) -> Vec<(Option<String>, serde_json::Value)> {
    sqlx::query_as(
        "SELECT subject_hash, details FROM security_audit_log \
         WHERE kind = $1 AND user_id = $2 ORDER BY created, id",
    )
    .bind(kind)
    .bind(user_id)
    .fetch_all(&app.db)
    .await
    .unwrap()
}

/// Issuing a token writes one `api_token.created` naming the token by its
/// hashed id, never by its value; revoking it writes one `api_token.revoked`
/// about the same token.
#[test]
fn creating_and_revoking_a_token_writes_one_event_each() {
    run_test(async {
        let app = shared_app().await;
        let (user_id, cookies, ip, csrf) = signed_in_user(app, "b223tok").await;
        // A label holding an address: free text its holder typed, which the
        // security log must not keep.
        let address = format!("b223-{}@example.com", Uuid::now_v7().simple());
        let name = format!("deploy key for {address}");

        let response = create(app, &cookies, &ip, &csrf, &name).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created: serde_json::Value = serde_json::from_str(&body_of(response).await).unwrap();
        let raw = created["token"].as_str().unwrap().to_string();
        let id = created["id"].as_str().unwrap().to_string();

        let issued = events(app, "api_token.created", user_id).await;
        assert_eq!(issued.len(), 1, "exactly one api_token.created: {issued:?}");
        let (subject, details) = &issued[0];
        assert_eq!(subject.as_deref(), Some(hash_subject(&id).as_str()));
        assert!(details.get("name").is_none(), "no label: {details}");
        assert_eq!(details["expires"], true);
        let stored: String = sqlx::query_scalar(
            "SELECT row_to_json(s)::text FROM security_audit_log s \
             WHERE kind = 'api_token.created' AND user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(!stored.contains(&raw), "never the token: {stored}");
        assert!(
            !stored.contains(&address),
            "never the label's contents: {stored}"
        );

        let csrf = page_csrf(app, &cookies, &ip).await;
        assert_eq!(
            revoke(app, &cookies, &ip, &csrf, &id).await,
            StatusCode::NO_CONTENT
        );
        let revoked = events(app, "api_token.revoked", user_id).await;
        assert_eq!(
            revoked.len(),
            1,
            "exactly one api_token.revoked: {revoked:?}"
        );
        let (subject, details) = &revoked[0];
        assert_eq!(subject.as_deref(), Some(hash_subject(&id).as_str()));
        assert_eq!(details["reason"], "self");
    });
}

/// A create refused for a missing CSRF token, and a revoke of a token that
/// is not the caller's, write nothing.
#[test]
fn a_refused_create_or_revoke_writes_no_event() {
    run_test(async {
        let app = shared_app().await;
        let (user_id, cookies, ip, csrf) = signed_in_user(app, "b223tokno").await;
        let (other_id, other_cookies, other_ip, other_csrf) =
            signed_in_user(app, "b223tokother").await;

        let response = create(app, &cookies, &ip, "not-the-token", "b223 refused").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(events(app, "api_token.created", user_id).await.is_empty());

        // The other user's token, which this user may not revoke.
        let response = create(app, &other_cookies, &other_ip, &other_csrf, "b223 theirs").await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let theirs: serde_json::Value = serde_json::from_str(&body_of(response).await).unwrap();
        let theirs = theirs["id"].as_str().unwrap().to_string();

        assert_eq!(
            revoke(app, &cookies, &ip, &csrf, &theirs).await,
            StatusCode::NOT_FOUND
        );
        assert!(events(app, "api_token.revoked", user_id).await.is_empty());
        assert!(
            events(app, "api_token.revoked", other_id).await.is_empty(),
            "the token was not revoked, so no event says it was"
        );
    });
}
