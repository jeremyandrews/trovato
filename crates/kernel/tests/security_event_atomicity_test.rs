#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A security event that records a stored change commits with that change
//! (B277): when the event cannot be written, the change does not persist and
//! the request does not report success.
//!
//! Before this, every such path committed its change and wrote its event
//! afterwards, on the pool, and a failed write was only logged. An account
//! could be created, a password changed, an account deleted or an API token
//! issued with no record of it in the security log.
//!
//! Each test makes the event insert fail with a trigger on
//! `security_audit_log` that raises for one `User-Agent` only, the one its own
//! request sends, so the tests running beside it on the shared database keep
//! writing their events. The trigger is dropped when the test ends, pass or
//! fail.
//!
//! Requires Postgres and Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::{RegistrationMode, SiteConfig};
use uuid::Uuid;

const PASSWORD: &str = "b277-old-password-1";
const NEW_PASSWORD: &str = "b277-new-password-2";

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

/// Make every security event a request with `user_agent` writes fail, until
/// the test ends. Returns nothing: the trigger is the effect.
async fn refuse_events_from(app: &TestApp, user_agent: &str) {
    let name = format!("b277_refuse_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'security event refused by the B277 test'; END $$"
    )))
    .execute(&app.db)
    .await
    .unwrap();
    // A user agent is free text a test chose: quoted, never interpolated raw.
    let quoted = user_agent.replace('\'', "''");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER {name} BEFORE INSERT ON security_audit_log FOR EACH ROW \
         WHEN (NEW.user_agent = '{quoted}') EXECUTE FUNCTION {name}()"
    )))
    .execute(&app.db)
    .await
    .unwrap();
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER IF EXISTS {name} ON security_audit_log"
        )))
        .execute(&db)
        .await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP FUNCTION IF EXISTS {name}()"
        )))
        .execute(&db)
        .await;
    });
}

/// A `User-Agent` no other request sends.
fn unique_agent(label: &str) -> String {
    format!("b277-{label}-{}", Uuid::now_v7().simple())
}

/// A fresh account, deleted when the test ends: `(id, name)`.
async fn account(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    app.create_test_user(&name, PASSWORD, &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM api_tokens WHERE user_id = $1")
            .bind(id)
            .execute(&db)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
    (id, name)
}

/// The value of the first input named `name` on the page.
fn input_value(html: &str, name: &str) -> String {
    common::input_value_in(html, name).unwrap_or_else(|| panic!("an input named {name}"))
}

/// The page's `csrf-token` meta tag.
fn meta_csrf(html: &str) -> String {
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("a csrf-token meta tag") + marker.len();
    html[start..].split('"').next().unwrap().to_string()
}

async fn get_page(app: &TestApp, cookies: &str, path: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{path} did not load");
    body_of(response).await
}

/// Open registration for the test, and put the settings back after.
async fn open_registration(app: &TestApp) {
    let mut saved = Vec::new();
    for key in ["user_registration", "allow_user_registration"] {
        saved.push((key, SiteConfig::get(&app.db, key).await.ok().flatten()));
    }
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        for (key, value) in saved {
            match value {
                Some(value) => {
                    let _ = SiteConfig::set(&db, key, value).await;
                }
                None => {
                    let _ = SiteConfig::delete(&db, key).await;
                }
            }
        }
    });
    RegistrationMode::Open
        .save(&app.db)
        .await
        .expect("open registration");
}

/// A registration whose `account.created` cannot be written creates no
/// account and does not answer as a registration that worked.
#[test]
fn a_registration_whose_event_fails_creates_no_account() {
    run_test(async {
        let app = shared_app().await;
        open_registration(app).await;
        let name = common::username("b277_reg");
        let ip = common::test_ip_for(&name);
        app.state.rate_limiter().reset("register", &ip).await.ok();
        let db = app.db.clone();
        let cleanup_name = name.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                .bind(&cleanup_name)
                .execute(&db)
                .await;
        });

        let agent = unique_agent("registration");
        refuse_events_from(app, &agent).await;
        let response = app
            .request(
                Request::post("/user/register/json")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::USER_AGENT, &agent)
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({
                            "username": name,
                            "mail": format!("{name}@example.com"),
                            "password": PASSWORD,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        let status = response.status();
        let body = body_of(response).await;

        assert!(
            !status.is_success(),
            "a registration whose event failed answered {status}: {body}"
        );
        let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(
            accounts, 0,
            "the account was created with no account.created event"
        );
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE recipient = $1")
                .bind(format!("{name}@example.com"))
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(queued, 0, "the verification mail was queued anyway");
    });
}

/// A password change whose `password.changed` cannot be written leaves the
/// old password in place and says the change failed.
#[test]
fn a_password_change_whose_event_fails_keeps_the_old_password() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_pw").await;
        let cookies = app.login(&name, PASSWORD).await;
        let html = get_page(app, &cookies, "/user/profile").await;
        let fields = common::form_fields(&html, "user_password_change");

        let agent = unique_agent("password");
        refuse_events_from(app, &agent).await;
        let response = app
            .request_with_cookies(
                Request::post("/user/password")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::USER_AGENT, &agent)
                    .body(Body::from(format!(
                        "{fields}&current_password={PASSWORD}&new_password={NEW_PASSWORD}\
                         &confirm_password={NEW_PASSWORD}"
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        let body = body_of(response).await;

        assert!(
            !body.contains("Password changed successfully."),
            "a password change whose event failed reported success"
        );
        assert!(
            body.contains("An error occurred while changing your password."),
            "a password change whose event failed did not say so"
        );
        let user = trovato_kernel::models::User::find_by_id(&app.db, id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            user.verify_password(PASSWORD),
            "the password changed with no password.changed event"
        );
        assert!(!user.verify_password(NEW_PASSWORD));
    });
}

/// An administrator's delete whose `account.deleted` cannot be written leaves
/// the account in place and answers with the delete's own failure.
#[test]
fn an_admin_delete_whose_event_fails_keeps_the_account() {
    run_test(async {
        let app = shared_app().await;
        let (target, _) = account(app, "b277_del").await;
        let admin_name = common::username("b277_del_admin");
        let admin = app
            .create_and_login_admin(&admin_name, PASSWORD, &format!("{admin_name}@example.com"))
            .await;
        let html = get_page(app, &admin, "/admin/people").await;
        let token = input_value(&html, "_token");

        let agent = unique_agent("delete");
        refuse_events_from(app, &agent).await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/people/{target}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::USER_AGENT, &agent)
                    .body(Body::from(format!("_token={}", urlencoding::encode(&token))))
                    .unwrap(),
                &admin,
            )
            .await;
        let status = response.status();
        let body = body_of(response).await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a delete whose event failed must answer as a failed delete: {body}"
        );
        assert!(body.contains("Failed to delete user."), "{body}");
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(target)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(exists, "the account was deleted with no account.deleted event");
    });
}

/// An API token whose `api_token.created` cannot be written is not issued:
/// no token row, and no token in the answer.
#[test]
fn an_api_token_whose_event_fails_is_not_issued() {
    run_test(async {
        let app = shared_app().await;
        let (id, name) = account(app, "b277_tok").await;
        let cookies = app.login(&name, PASSWORD).await;
        let html = get_page(app, &cookies, "/user/tokens").await;
        let csrf = meta_csrf(&html);

        let agent = unique_agent("token");
        refuse_events_from(app, &agent).await;
        let response = app
            .request_with_cookies(
                Request::post("/api/tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::USER_AGENT, &agent)
                    .header("x-csrf-token", csrf)
                    .body(Body::from(
                        serde_json::json!({ "name": "b277", "expires_in_days": 30 }).to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = body_of(response).await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a token whose event failed must answer as a failed create: {body}"
        );
        assert!(!body.contains("\"token\""), "the raw token was handed out: {body}");
        let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens WHERE user_id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(tokens, 0, "the token was issued with no api_token.created event");
    });
}
