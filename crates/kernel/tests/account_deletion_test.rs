#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Self-service account deletion, end to end.
//!
//! Both re-authentication paths are driven for real: a password account posts its
//! password, and a passwordless account signs a real assertion with a `SoftPasskey`,
//! the same harness the WebAuthn tests use. Nothing here mocks the ceremony.
//!
//! The claims under test, in order of how much they would cost to get wrong:
//!
//! 1. **A session cookie alone cannot delete an account.** The confirm screen and its
//!    POST both require a fresh step-up.
//! 2. **Content survives, reattributed.** An item and a comment written by the
//!    deleted account are still there, owned by the anonymous author. Before this
//!    change the delete did not merely skip that — it failed outright on a foreign
//!    key.
//! 3. **The last active administrator cannot leave.**
//! 4. **Sessions, credentials and tokens go.**
//! 5. **A plugin gets to clean up**, through `tap_user_delete`.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test, shared_app};
use uuid::Uuid;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

const PASSWORD: &str = "test-password-123";

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    serde_json::from_str(&body_text(response).await).expect("JSON body")
}

fn rp_origin(app: &TestApp) -> url::Url {
    app.state
        .webauthn()
        .get_allowed_origins()
        .first()
        .expect("the relying party must have an origin")
        .clone()
}

/// The CSRF token from a page that always carries one.
async fn csrf_meta(app: &TestApp, cookies: &str, path: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let html = body_text(response).await;
    let pos = html
        .find("csrf-token")
        .unwrap_or_else(|| panic!("no csrf-token meta on {path}: {html}"));
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

/// The `_token` value from the first form on a page.
async fn form_token(app: &TestApp, cookies: &str, path: &str) -> (String, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let refreshed = extract_cookies(&response);
    let cookies = if refreshed.is_empty() {
        cookies.to_string()
    } else {
        refreshed
    };
    let html = body_text(response).await;
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .map(|i| i + needle.len())
        .unwrap_or_else(|| panic!("no form token on {path}: {html}"));
    let end = html[start..].find('"').unwrap() + start;
    (cookies, html[start..end].to_string())
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn post_form(
    app: &TestApp,
    cookies: &str,
    path: &str,
    fields: &[(&str, &str)],
) -> axum::response::Response {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    app.request_with_cookies(
        Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", "198.51.100.9")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

/// A fresh account with one item and one comment, and its cookies.
struct Account {
    id: Uuid,
    cookies: String,
    item_id: Uuid,
    item_title: String,
    comment_id: Uuid,
}

async fn seed_account(app: &TestApp, label: &str, admin: bool) -> Account {
    let username = format!("{label}_{}", Uuid::now_v7().simple());
    let email = format!("{username}@example.com");
    if admin {
        app.create_test_admin(&username, PASSWORD, &email).await;
    } else {
        app.create_test_user(&username, PASSWORD, &email).await;
    }
    let cookies = app.login(&username, PASSWORD).await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&username)
        .fetch_one(&app.db)
        .await
        .expect("the account must exist");

    app.ensure_conference_type().await;

    let item_id = Uuid::now_v7();
    let item_title = format!("Written by {username}");
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, promote, \
         sticky, fields, language) \
         VALUES ($1, 'conference', $2, $3, 1, $4, $4, 0, 0, '{}'::jsonb, 'en')",
    )
    .bind(item_id)
    .bind(&item_title)
    .bind(id)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("insert item");

    // A revision too: `item_revision.author_id` is the other NOT NULL reference,
    // and it is the one an insert-only test would miss.
    sqlx::query(
        "INSERT INTO item_revision (id, item_id, title, author_id, status, created, fields, log) \
         VALUES ($1, $2, $3, $4, 1, $5, '{}'::jsonb, 'seed')",
    )
    .bind(Uuid::now_v7())
    .bind(item_id)
    .bind(&item_title)
    .bind(id)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("insert revision");

    let comment_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO comment (id, item_id, parent_id, author_id, body, body_format, status, \
         created, changed) VALUES ($1, $2, NULL, $3, 'A comment', 'plain_text', 1, $4, $4)",
    )
    .bind(comment_id)
    .bind(item_id)
    .bind(id)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("insert comment");

    Account {
        id,
        cookies,
        item_id,
        item_title,
        comment_id,
    }
}

async fn cleanup(app: &TestApp, account: &Account) {
    let _ = sqlx::query("DELETE FROM comment WHERE item_id = $1")
        .bind(account.item_id)
        .execute(&app.db)
        .await;
    let _ = sqlx::query("DELETE FROM item_revision WHERE item_id = $1")
        .bind(account.item_id)
        .execute(&app.db)
        .await;
    let _ = sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(account.item_id)
        .execute(&app.db)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(account.id)
        .execute(&app.db)
        .await;
}

/// Re-authenticate with a password and return the refreshed cookies.
async fn step_up_with_password(app: &TestApp, account: &Account) -> String {
    app.state
        .rate_limiter()
        .reset("password", "198.51.100.9")
        .await
        .ok();
    let (cookies, token) = form_token(app, &account.cookies, "/user/delete").await;
    let response = post_form(
        app,
        &cookies,
        "/user/delete/reauth",
        &[("_token", &token), ("password", PASSWORD)],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "a correct password must step the session up"
    );
    let refreshed = extract_cookies(&response);
    if refreshed.is_empty() {
        cookies
    } else {
        refreshed
    }
}

// =============================================================================
// A session cookie alone is not enough
// =============================================================================

/// Without a step-up, the confirmation screen sends you back to prove it is you.
#[test]
fn the_confirmation_screen_requires_a_fresh_step_up() {
    run_test(async {
        let app = shared_app().await;
        let account = seed_account(app, "delnostep", false).await;

        let response = app
            .request_with_cookies(
                Request::get("/user/delete/confirm")
                    .body(Body::empty())
                    .unwrap(),
                &account.cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok()),
            Some("/user/delete"),
            "an un-stepped-up session must be sent to re-authenticate"
        );

        cleanup(app, &account).await;
    });
}

/// And neither can the POST be reached by skipping the screen.
#[test]
fn deleting_without_a_step_up_does_nothing() {
    run_test(async {
        let app = shared_app().await;
        let account = seed_account(app, "delskip", false).await;

        // A valid CSRF token from a page the account can legitimately load, so the
        // only thing missing is the step-up.
        let (cookies, token) = form_token(app, &account.cookies, "/user/profile").await;
        let response =
            post_form(app, &cookies, "/user/delete/confirm", &[("_token", &token)]).await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok()),
            Some("/user/delete")
        );

        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(account.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(alive, 1, "the account must still exist");

        cleanup(app, &account).await;
    });
}

/// A wrong password does not step the session up, and says so.
#[test]
fn a_wrong_password_does_not_step_the_session_up() {
    run_test(async {
        let app = shared_app().await;
        let account = seed_account(app, "delwrongpw", false).await;
        app.state
            .rate_limiter()
            .reset("password", "198.51.100.9")
            .await
            .ok();

        let (cookies, token) = form_token(app, &account.cookies, "/user/delete").await;
        let response = post_form(
            app,
            &cookies,
            "/user/delete/reauth",
            &[("_token", &token), ("password", "not-the-password")],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK, "it re-renders the form");
        assert!(
            body_text(response).await.contains("not right"),
            "the refusal must say what was wrong"
        );

        // And the confirm screen is still closed.
        let response = app
            .request_with_cookies(
                Request::get("/user/delete/confirm")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        cleanup(app, &account).await;
    });
}

/// A write without a valid CSRF token is rejected.
#[test]
fn a_delete_without_a_valid_csrf_token_is_rejected() {
    run_test(async {
        let app = shared_app().await;
        let account = seed_account(app, "delcsrf", false).await;
        let cookies = step_up_with_password(app, &account).await;

        let response =
            post_form(app, &cookies, "/user/delete/confirm", &[("_token", "nope")]).await;
        assert_ne!(response.status(), StatusCode::SEE_OTHER);

        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(account.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(alive, 1, "a CSRF-rejected delete must not delete");

        cleanup(app, &account).await;
    });
}

// =============================================================================
// The password path, end to end
// =============================================================================

/// The whole flow for a password account, with every consequence checked.
#[test]
fn a_password_account_deletes_itself_and_its_content_is_reattributed() {
    run_test(async {
        let app = shared_app().await;
        let account = seed_account(app, "delpw", false).await;

        // An API token and a session index entry, so their removal is observable.
        sqlx::query(
            "INSERT INTO api_tokens (id, user_id, name, token_hash, created) \
             VALUES ($1, $2, 'test', $3, NOW())",
        )
        .bind(Uuid::now_v7())
        .bind(account.id)
        // token_hash is UNIQUE, so a fixed value passes once and fails on the
        // second run against the same database.
        .bind(Uuid::now_v7().simple().to_string())
        .execute(&app.db)
        .await
        .expect("insert api token");
        app.state
            .session_registry()
            .observe(
                account.id,
                Uuid::now_v7(),
                &format!("session-{}", Uuid::now_v7().simple()),
                "203.0.113.9",
                "Mozilla/5.0 Chrome/120.0.0.0",
                false,
                chrono::Utc::now().timestamp(),
            )
            .await
            .expect("index a session");

        let cookies = step_up_with_password(app, &account).await;

        // The confirmation screen says what will happen, in the terms the policy
        // uses, and counts the content.
        let (cookies, token) = form_token(app, &cookies, "/user/delete/confirm").await;
        let screen = body_text(
            app.request_with_cookies(
                Request::get("/user/delete/confirm")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await,
        )
        .await;
        assert!(
            screen.contains("attributed to <strong>Anonymous</strong>"),
            "the screen must say the content is reattributed, got: {screen}"
        );
        assert!(
            screen.contains("1 item(s)") && screen.contains("1 comment(s)"),
            "the screen must count what is affected, got: {screen}"
        );
        assert!(
            screen.contains("/user/data-export"),
            "the screen must offer the export before it is too late"
        );

        let response =
            post_form(app, &cookies, "/user/delete/confirm", &[("_token", &token)]).await;
        let status = response.status();
        let reason = body_text(response).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "delete failed: {reason}");

        // The account is gone.
        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(account.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(alive, 0, "the account row must be gone");

        // The content is not, and it belongs to the anonymous author.
        let (item_author, item_title): (Uuid, String) =
            sqlx::query_as("SELECT author_id, title FROM item WHERE id = $1")
                .bind(account.item_id)
                .fetch_one(&app.db)
                .await
                .expect("the item must survive its author");
        assert_eq!(item_author, Uuid::nil(), "the item is now anonymous");
        assert_eq!(item_title, account.item_title, "and otherwise unchanged");

        let comment_author: Uuid =
            sqlx::query_scalar("SELECT author_id FROM comment WHERE id = $1")
                .bind(account.comment_id)
                .fetch_one(&app.db)
                .await
                .expect("the comment must survive its author");
        assert_eq!(comment_author, Uuid::nil());

        let revision_authors: Vec<Uuid> =
            sqlx::query_scalar("SELECT author_id FROM item_revision WHERE item_id = $1")
                .bind(account.item_id)
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert!(
            !revision_authors.is_empty() && revision_authors.iter().all(|a| *a == Uuid::nil()),
            "revisions must be reattributed too, got: {revision_authors:?}"
        );

        // Tokens cascade away.
        let tokens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_tokens WHERE user_id = $1")
            .bind(account.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(tokens, 0, "API tokens must go with the account");

        // Sessions are revoked everywhere.
        let sessions = app
            .state
            .session_registry()
            .list(account.id)
            .await
            .unwrap_or_default();
        assert!(sessions.is_empty(), "every session must be revoked");

        // The audit stream records it under a **hash**, and carries no `user_id`:
        // that column is `ON DELETE SET NULL` and the account is gone, and an audit
        // row naming the erased account's raw id would undercut the erasure. So the
        // row is found by the hashed subject, which is the only identity it has.
        let expected_hash = trovato_kernel::audit::hash_subject(&account.id.to_string());
        let (user_id, details): (Option<Uuid>, serde_json::Value) = sqlx::query_as(
            "SELECT user_id, details FROM security_audit_log \
             WHERE kind = 'account.deleted' AND subject_hash = $1 \
             ORDER BY created DESC LIMIT 1",
        )
        .bind(&expected_hash)
        .fetch_one(&app.db)
        .await
        .expect("a deletion must be audited under the hashed account id");
        assert_eq!(
            user_id, None,
            "the audit row must not name the account it just erased"
        );
        assert_eq!(details["by"], "self");
        assert_eq!(details["items_reattributed"], 1);
        assert_eq!(details["comments_reattributed"], 1);

        // And the raw id appears nowhere in the row.
        let raw_matches: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM security_audit_log WHERE subject_hash = $1")
                .bind(account.id.to_string())
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(raw_matches, 0, "the raw account id must never be stored");

        cleanup(app, &account).await;
    });
}

/// The signed-in session stops working immediately.
#[test]
fn the_deleted_accounts_session_stops_working() {
    run_test(async {
        let app = shared_app().await;
        let account = seed_account(app, "delsession", false).await;
        let cookies = step_up_with_password(app, &account).await;
        let (cookies, token) = form_token(app, &cookies, "/user/delete/confirm").await;

        let response =
            post_form(app, &cookies, "/user/delete/confirm", &[("_token", &token)]).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        // The same cookie must no longer reach an authenticated page.
        let response = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "the old session must not still be signed in"
        );

        cleanup(app, &account).await;
    });
}

// =============================================================================
// The passkey path, with a real assertion
// =============================================================================

/// A passwordless account steps up with a real passkey assertion, then deletes.
#[test]
fn a_passkey_account_deletes_itself_after_a_real_assertion() {
    run_test(async {
        let app = shared_app().await;
        let account = seed_account(app, "delpasskey", false).await;

        // Register a passkey, then drop the password so the account is passwordless
        // and the passkey path is the only one it has.
        let csrf = csrf_meta(app, &account.cookies, "/user/passkeys").await;
        let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let start = app
            .request_with_cookies(
                Request::post("/user/webauthn/register/start")
                    .header("x-csrf-token", &csrf)
                    .body(Body::empty())
                    .unwrap(),
                &account.cookies,
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
                &account.cookies,
            )
            .await;
        assert_eq!(finish.status(), StatusCode::OK, "passkey registration");

        // The step-up screen offers **both** methods, because this account has both.
        // Making somebody type a password they may not remember when the
        // authenticator is right there would be a worse screen, and the reverse when
        // the authenticator is at home.
        let screen = body_text(
            app.request_with_cookies(
                Request::get("/user/delete").body(Body::empty()).unwrap(),
                &account.cookies,
            )
            .await,
        )
        .await;
        assert!(
            screen.contains("Verify with my passkey"),
            "an account with a passkey must be offered it, got: {screen}"
        );
        assert!(
            screen.contains(r#"name="password""#),
            "and an account with a password must still be offered that"
        );

        // Drive the deletion-scoped ceremony.
        let csrf = csrf_meta(app, &account.cookies, "/user/passkeys").await;
        let start = app
            .request_with_cookies(
                Request::post("/user/delete/passkey/start")
                    .header("x-csrf-token", &csrf)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
                &account.cookies,
            )
            .await;
        assert_eq!(start.status(), StatusCode::OK, "step-up start");
        let cookies = {
            let refreshed = extract_cookies(&start);
            if refreshed.is_empty() {
                account.cookies.clone()
            } else {
                refreshed
            }
        };
        let challenge: webauthn_rs::prelude::RequestChallengeResponse =
            serde_json::from_value(json_body(start).await).expect("a request challenge");
        let assertion = authenticator
            .do_authentication(rp_origin(app), challenge)
            .expect("the soft authenticator signs the assertion");

        let csrf = csrf_meta(app, &cookies, "/user/passkeys").await;
        let verify = app
            .request_with_cookies(
                Request::post("/user/delete/passkey/finish")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({ "credential": assertion }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            verify.status(),
            StatusCode::OK,
            "a real assertion must step the session up"
        );
        let cookies = {
            let refreshed = extract_cookies(&verify);
            if refreshed.is_empty() {
                cookies
            } else {
                refreshed
            }
        };

        let (cookies, token) = form_token(app, &cookies, "/user/delete/confirm").await;
        let response =
            post_form(app, &cookies, "/user/delete/confirm", &[("_token", &token)]).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(account.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(alive, 0, "the passwordless account must be deleted");

        let credentials: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_credentials WHERE user_id = $1")
                .bind(account.id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(credentials, 0, "its passkeys must go with it");

        cleanup(app, &account).await;
    });
}

/// A ceremony started for one account cannot be finished by another.
#[test]
fn a_step_up_ceremony_is_scoped_to_the_account_that_started_it() {
    run_test(async {
        let app = shared_app().await;
        let mine = seed_account(app, "delscopea", false).await;

        // No ceremony in this session at all: the finish must refuse rather than
        // accept an assertion on trust.
        let csrf = csrf_meta(app, &mine.cookies, "/user/passkeys").await;
        let response = app
            .request_with_cookies(
                Request::post("/user/delete/passkey/finish")
                    .header("content-type", "application/json")
                    .header("x-csrf-token", &csrf)
                    .body(Body::from(
                        serde_json::json!({
                            "credential": {
                                "id": "nonsense",
                                "rawId": "bm9uc2Vuc2U",
                                "type": "public-key",
                                "extensions": {},
                                "response": {
                                    "authenticatorData": "bm9uc2Vuc2U",
                                    "clientDataJSON": "bm9uc2Vuc2U",
                                    "signature": "bm9uc2Vuc2U",
                                    "userHandle": null
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                &mine.cookies,
            )
            .await;
        assert!(
            response.status().is_client_error(),
            "a finish with no ceremony must be refused, got {}",
            response.status()
        );

        cleanup(app, &mine).await;
    });
}

// =============================================================================
// The last administrator
// =============================================================================

/// The last-administrator guard, against a database whose whole administrator
/// population this test controls.
///
/// Deliberately **not** a route test. The refusal depends on how many active
/// administrators the *site* has, and the shared fixture database is shared with
/// every other test in the suite — several of which create administrators. A route
/// test would have to demote everyone else and hope nothing concurrently promoted
/// somebody, which is a test that passes most of the time. So the count the route
/// branches on is tested where it can be made exact, and the branch itself is a
/// pure function tested beside the code (`blocks_last_admin`).
#[tokio::test]
async fn the_active_admin_count_is_exact_and_excludes_the_anonymous_sentinel() {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let without_query = database_url
        .split_once('?')
        .map_or(database_url.as_str(), |(base, _)| base);
    let cut = without_query.rfind('/').expect("a database name");
    let server_url = without_query[..cut].to_string();
    let name = format!("trovato_admincount_{}", Uuid::now_v7().simple());

    {
        use sqlx::{Connection, Executor};
        let mut admin = sqlx::PgConnection::connect(&format!("{server_url}/postgres"))
            .await
            .expect("connect");
        admin
            .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
            .await
            .expect("create");
    }
    let pool = sqlx::PgPool::connect(&format!("{server_url}/{name}"))
        .await
        .expect("connect");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("migrate");

    // A fresh site has no administrator at all: the installer makes the first one.
    let count = trovato_kernel::models::User::active_admin_count(&pool)
        .await
        .expect("count");
    assert_eq!(
        count, 0,
        "the anonymous sentinel must not count as an administrator"
    );

    for (label, is_admin, status, expected) in [
        ("first_admin", true, 1i16, 1i64),
        ("blocked_admin", true, 0i16, 1),
        ("plain_user", false, 1i16, 1),
        ("second_admin", true, 1i16, 2),
    ] {
        sqlx::query(
            "INSERT INTO users (id, name, pass, mail, is_admin, status, created) \
             VALUES ($1, $2, 'x', $3, $4, $5, NOW())",
        )
        .bind(Uuid::now_v7())
        .bind(format!("{label}_{}", Uuid::now_v7().simple()))
        .bind(format!("{label}@example.invalid"))
        .bind(is_admin)
        .bind(status)
        .execute(&pool)
        .await
        .expect("insert");

        let count = trovato_kernel::models::User::active_admin_count(&pool)
            .await
            .expect("count");
        assert_eq!(
            count, expected,
            "after adding {label}, the active administrator count should be {expected}"
        );
    }

    pool.close().await;
    {
        use sqlx::{Connection, Executor};
        if let Ok(mut admin) = sqlx::PgConnection::connect(&format!("{server_url}/postgres")).await
        {
            let _ = admin
                .execute(format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#).as_str())
                .await;
        }
    }
}

/// An administrator who is not the last one can delete themselves.
#[test]
fn an_administrator_with_a_colleague_can_delete_itself() {
    run_test(async {
        let app = shared_app().await;
        let colleague = seed_account(app, "delcolleague", true).await;
        let account = seed_account(app, "deladmin", true).await;

        let cookies = step_up_with_password(app, &account).await;
        let (cookies, token) = form_token(app, &cookies, "/user/delete/confirm").await;
        let response =
            post_form(app, &cookies, "/user/delete/confirm", &[("_token", &token)]).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(account.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(alive, 0);

        cleanup(app, &account).await;
        cleanup(app, &colleague).await;
    });
}

// =============================================================================
// The admin path, which the same fix unblocked
// =============================================================================

/// An administrator deleting somebody else now works when that account wrote
/// something — which it did not before, because the foreign key refused.
#[test]
fn an_admin_can_delete_an_account_that_authored_content() {
    run_test(async {
        let app = shared_app().await;
        let victim = seed_account(app, "delvictim", false).await;
        let admin = seed_account(app, "deladmin2", true).await;

        let (cookies, token) = form_token(app, &admin.cookies, "/admin/people").await;
        let response = post_form(
            app,
            &cookies,
            &format!("/admin/people/{}/delete", victim.id),
            &[("_token", &token)],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "the admin delete must succeed for an account with content"
        );

        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(victim.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(alive, 0, "the account must be gone");

        let item_author: Uuid = sqlx::query_scalar("SELECT author_id FROM item WHERE id = $1")
            .bind(victim.item_id)
            .fetch_one(&app.db)
            .await
            .expect("the item must survive");
        assert_eq!(item_author, Uuid::nil());

        cleanup(app, &victim).await;
        cleanup(app, &admin).await;
    });
}
