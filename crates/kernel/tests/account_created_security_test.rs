#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `account.created`: every path that makes an account writes one security
//! event for it.
//!
//! Before this, account creation wrote no security event at all — on any
//! path. `SecurityEventKind` had no kind for it, so public registration (form
//! and JSON), the admin "Add user" form, the installer and `trovato user
//! create` left the security audit stream with no record that an account had
//! appeared, only the content audit log's generic `create`/`user` row (and
//! that only when the optional `trovato_audit_log` plugin is enabled).
//!
//! The installer and `trovato user create` are covered by
//! `zz_operator_cli_test.rs`, which already runs the real binary; this file
//! covers the paths reachable through the shared `TestApp` router
//! (registration by form and by JSON, and the admin "Add user" form).
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::{RegistrationMode, SiteConfig};
use uuid::Uuid;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    body: String,
) -> axum::response::Response {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The token and build id of the form whose `action` is `action`.
fn form_pairs(html: &str, action: &str) -> String {
    let at = html
        .find(&format!(r#"action="{action}""#))
        .unwrap_or_else(|| panic!("a form posting to {action}"));
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    let scoped = &scoped[..scoped.find("</form>").unwrap()];
    let mut pairs = format!(
        "_token={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap())
    );
    if let Some(build) = input_value(scoped, "_form_build_id") {
        pairs.push_str(&format!("&_form_build_id={}", urlencoding::encode(&build)));
    }
    pairs
}

async fn admin_cookies(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let cookies = app
        .create_and_login_admin(
            &name,
            "correct-horse-battery-staple",
            &format!("{name}@test.local"),
        )
        .await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .unwrap();
    (id, cookies)
}

fn delete_user_on_exit(app: &TestApp, name: &str) {
    let db = app.db.clone();
    let name = name.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&name)
            .execute(&db)
            .await;
    });
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

/// One `account.created` row for `user_id`, with its `details`.
async fn account_created_event(app: &TestApp, user_id: Uuid) -> Option<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT details FROM security_audit_log WHERE kind = 'account.created' AND user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&app.db)
    .await
    .unwrap()
}

async fn account_created_count(app: &TestApp, user_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM security_audit_log WHERE kind = 'account.created' AND user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&app.db)
    .await
    .unwrap()
}

/// Public registration (the JSON endpoint) writes exactly one
/// `account.created` event, naming the path, that the account starts
/// inactive, and that nobody but the registrant did it.
#[test]
fn registration_writes_one_account_created_event() {
    run_test(async {
        let app = shared_app().await;
        open_registration(app).await;

        let name = common::username("acctcreatedreg");
        delete_user_on_exit(app, &name);
        let ip = common::test_ip_for(&name);
        app.state.rate_limiter().reset("register", &ip).await.ok();

        let response = app
            .request(
                Request::post("/user/register/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(
                        serde_json::json!({
                            "username": name,
                            "mail": format!("{name}@example.com"),
                            "password": "correct-horse-battery-staple",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            body_string(response).await
        );

        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .expect("the account exists");

        assert_eq!(
            account_created_count(app, user_id).await,
            1,
            "exactly one account.created event"
        );
        let details = account_created_event(app, user_id)
            .await
            .expect("an account.created row");
        assert_eq!(details["path"], "registration");
        assert_eq!(details["active"], false, "pending e-mail verification");
        assert_eq!(details["by"], "self");
    });
}

/// Public registration through the HTML form (`POST /user/register`, the
/// route a visitor's browser uses) writes the same single `account.created`
/// event as the JSON endpoint: both call the same `do_register`, which calls
/// `UserService`, and the event is written there.
#[test]
fn registration_form_writes_one_account_created_event() {
    run_test(async {
        let app = shared_app().await;
        open_registration(app).await;

        let name = common::username("acctcreatedform");
        delete_user_on_exit(app, &name);
        let ip = common::test_ip_for(&name);
        app.state.rate_limiter().reset("register", &ip).await.ok();

        let page = app
            .request(
                Request::get("/user/register")
                    .header("x-forwarded-for", &ip)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let cookies = common::extract_cookies(&page);
        let html = body_string(page).await;
        let pairs = form_pairs(&html, "/user/register");

        let response = app
            .request_with_cookies(
                Request::post("/user/register")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(format!(
                        "{pairs}&username={}&mail={}\
                         &password=correct-horse-battery-staple\
                         &confirm_password=correct-horse-battery-staple",
                        urlencoding::encode(&name),
                        urlencoding::encode(&format!("{name}@example.com"))
                    )))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        let body = body_string(response).await;
        assert!(
            status.is_success() || status.is_redirection(),
            "the form must accept the registration: {status} {body}"
        );

        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .unwrap_or_else(|e| panic!("the account exists ({e}): {body}"));

        assert_eq!(
            account_created_count(app, user_id).await,
            1,
            "exactly one account.created event"
        );
        let details = account_created_event(app, user_id)
            .await
            .expect("an account.created row");
        assert_eq!(details["path"], "registration");
        assert_eq!(details["active"], false, "pending e-mail verification");
        assert_eq!(details["by"], "self");
    });
}

/// A registration refused for a taken username never reaches the account
/// layer, so it writes no `account.created` event.
#[test]
fn a_refused_registration_writes_no_account_created_event() {
    run_test(async {
        let app = shared_app().await;
        open_registration(app).await;

        let name = common::username("acctcreatedrefused");
        delete_user_on_exit(app, &name);
        let ip = common::test_ip_for(&name);
        app.state.rate_limiter().reset("register", &ip).await.ok();

        let body = |mail: &str| {
            serde_json::json!({
                "username": name,
                "mail": mail,
                "password": "correct-horse-battery-staple",
            })
            .to_string()
        };

        let first = app
            .request(
                Request::post("/user/register/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(body(&format!("{name}@example.com"))))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            first.status(),
            StatusCode::OK,
            "{}",
            body_string(first).await
        );
        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .expect("the first registration created the account");
        assert_eq!(account_created_count(app, user_id).await, 1);

        app.state.rate_limiter().reset("register", &ip).await.ok();
        let second = app
            .request(
                Request::post("/user/register/json")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", &ip)
                    .body(Body::from(body(&format!("other-{name}@example.com"))))
                    .unwrap(),
            )
            .await;
        assert!(
            second.status().is_client_error(),
            "a taken username must be refused, got {}",
            second.status()
        );

        assert_eq!(
            account_created_count(app, user_id).await,
            1,
            "the refused second attempt must not add a second event"
        );
    });
}

/// The admin "Add user" form writes exactly one `account.created` event,
/// naming the acting administrator and that the account starts active.
#[test]
fn admin_add_user_writes_one_account_created_event() {
    run_test(async {
        let app = shared_app().await;
        let (admin_id, cookies) = admin_cookies(app, "acctcreatedadm").await;

        let (status, html) = get(app, &cookies, "/admin/people/add").await;
        assert_eq!(status, StatusCode::OK);
        let pairs = form_pairs(&html, "/admin/people/add");
        let name = common::username("acctcreatedadded");
        delete_user_on_exit(app, &name);

        let response = post_form(
            app,
            &cookies,
            "/admin/people/add",
            format!(
                "{pairs}&name={name}&mail={}&password=correct-horse-battery-staple&status=1",
                urlencoding::encode(&format!("{name}@example.com"))
            ),
        )
        .await;
        assert!(
            response.status().is_redirection(),
            "the account must be created: {}",
            response.status()
        );

        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .expect("the account exists");

        assert_eq!(account_created_count(app, user_id).await, 1);
        let (actor_id, details): (Option<Uuid>, serde_json::Value) = sqlx::query_as(
            "SELECT actor_id, details FROM security_audit_log \
             WHERE kind = 'account.created' AND user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(actor_id, Some(admin_id));
        assert_eq!(details["path"], "admin");
        assert_eq!(details["active"], true);
        assert_eq!(details["by"], admin_id.to_string());
    });
}
