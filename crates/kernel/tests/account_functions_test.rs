#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Account functions a Drupal site offers (B156).
//!
//! Sign in with the e-mail address; registration that names a taken name;
//! registration with administrator approval; "Notify user of new account";
//! the account cancel page with Drupal's four methods; and the account page,
//! public to those allowed to read profiles.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, RegistrationMode, SiteConfig, UpdateUser};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const BROWSER: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const PASSWORD: &str = "test-password-123";

struct Answer {
    status: StatusCode,
    location: Option<String>,
    cookies: String,
    body: String,
}

async fn answer(response: axum::response::Response) -> Answer {
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cookies = common::extract_cookies(&response);
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Answer {
        status,
        location,
        cookies,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> Answer {
    let request = Request::get(path)
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", test_ip_for("b156-accounts"))
        .body(Body::empty())
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

async fn post_form(app: &TestApp, path: &str, cookies: &str, body: String, ip: &str) -> Answer {
    let request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", ip)
        .body(Body::from(body))
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

fn hidden(page: &str, name: &str) -> String {
    let marker = format!(r#"name="{name}" value=""#);
    let start = page
        .find(&marker)
        .unwrap_or_else(|| panic!("no {name}: {page}"))
        + marker.len();
    let end = page[start..].find('"').unwrap();
    page[start..start + end].to_string()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn delete_user_on_exit(app: &TestApp, id: Uuid) {
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
}

fn delete_user_named_on_exit(app: &TestApp, name: &str) {
    let db = app.db.clone();
    let name = name.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&name)
            .execute(&db)
            .await;
    });
}

/// A plain user, created and cleaned up; returns (id, name, cookies).
async fn member(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String) {
    let (id, cookies) = user_holding(app, prefix, permissions).await;
    delete_user_on_exit(app, id);
    (id, cookies)
}

async fn admin(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let cookies = app
        .create_and_login_admin(&name, PASSWORD, &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    delete_user_on_exit(app, id);
    (id, cookies)
}

/// Put both registration settings back when the test ends.
fn restore_registration_on_exit(
    saved: Vec<(&'static str, Option<serde_json::Value>)>,
    app: &TestApp,
) {
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
}

async fn set_registration(app: &TestApp, mode: RegistrationMode) {
    let mut saved = Vec::new();
    for key in ["user_registration", "allow_user_registration"] {
        saved.push((key, SiteConfig::get(&app.db, key).await.ok().flatten()));
    }
    restore_registration_on_exit(saved, app);
    mode.save(&app.db).await.unwrap();
}

async fn login_json(app: &TestApp, identifier: &str, password: &str) -> StatusCode {
    let ip = test_ip_for(identifier);
    app.state.lockout().clear_all(identifier).await.ok();
    app.state.rate_limiter().reset("login", &ip).await.ok();
    app.request(
        Request::post("/user/login/json")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({"username": identifier, "password": password}).to_string(),
            ))
            .unwrap(),
    )
    .await
    .status()
}

#[test]
fn an_account_signs_in_with_its_email_address() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b156-mail");
        let mail = format!("{name}@Example.com");
        app.create_test_user(&name, PASSWORD, &mail).await;
        delete_user_named_on_exit(app, &name);

        assert_eq!(login_json(app, &mail, PASSWORD).await, StatusCode::OK);
        assert_eq!(
            login_json(app, &mail.to_lowercase(), PASSWORD).await,
            StatusCode::OK,
            "the address is matched without regard to case"
        );
        // A wrong password, and an address nobody has, fail alike.
        assert_eq!(
            login_json(app, &mail, "wrong-password-123").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            login_json(app, "nobody-b156@example.com", PASSWORD).await,
            StatusCode::UNAUTHORIZED
        );
    });
}

#[test]
fn registration_names_a_taken_name_and_approval_keeps_the_account_blocked() {
    run_test(async {
        let app = shared_app().await;
        set_registration(app, RegistrationMode::Approval).await;

        // A taken name is named.
        let (_, _) = member(app, "b156-taken", &[]).await;
        let taken: String = sqlx::query_scalar(
            "SELECT name FROM users WHERE name LIKE 'b156-taken-%' ORDER BY created DESC LIMIT 1",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        let form = get(app, "/user/register", "").await;
        assert_eq!(form.status, StatusCode::OK, "{}", form.body);
        let token = hidden(&form.body, "_token");
        let ip = test_ip_for("b156-register");
        app.state.rate_limiter().reset("register", &ip).await.ok();
        let body = format!(
            "username={}&mail={}&password=long-enough-pass-1&confirm_password=long-enough-pass-1&_token={token}",
            enc(&taken),
            enc("b156-fresh@example.com")
        );
        let a = post_form(app, "/user/register", &form.cookies, body, &ip).await;
        assert!(
            a.body
                .contains(&format!("The name {taken} is already taken.")),
            "{}",
            a.body
        );

        // A fresh applicant is told the account waits for approval.
        let name = common::username("b156-applicant");
        delete_user_named_on_exit(app, &name);
        let form = get(app, "/user/register", "").await;
        let token = hidden(&form.body, "_token");
        app.state.rate_limiter().reset("register", &ip).await.ok();
        let body = format!(
            "username={}&mail={}&password=long-enough-pass-1&confirm_password=long-enough-pass-1&_token={token}",
            enc(&name),
            enc(&format!("{name}@example.com"))
        );
        let a = post_form(app, "/user/register", &form.cookies, body, &ip).await;
        assert!(
            a.body
                .contains("Your account is currently pending approval by the site administrator."),
            "{}",
            a.body
        );
        let user = app
            .state
            .users()
            .find_by_name(&name)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.status, 0);

        // Confirming the address leaves it blocked.
        let (_, plain) =
            trovato_kernel::models::email_verification::EmailVerificationToken::create(
                &app.db,
                user.id,
                trovato_kernel::models::email_verification::PURPOSE_REGISTRATION,
            )
            .await
            .unwrap();
        let a = get(app, &format!("/user/verify/{plain}"), "").await;
        assert!(a.status.is_redirection(), "{}", a.body);
        app.state.users().invalidate(user.id);
        let user = app
            .state
            .users()
            .find_by_id(user.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            user.status, 0,
            "an administrator activates it, not the link"
        );
        assert_eq!(
            login_json(app, &name, "long-enough-pass-1").await,
            StatusCode::UNAUTHORIZED
        );

        // The administrator's activation is what lets it in.
        app.state
            .users()
            .update(
                user.id,
                UpdateUser {
                    status: Some(1),
                    ..Default::default()
                },
                &UserContext::administrator(Uuid::nil(), vec![]),
            )
            .await
            .unwrap();
        assert_eq!(
            login_json(app, &name, "long-enough-pass-1").await,
            StatusCode::OK
        );
    });
}

#[test]
fn notify_user_of_new_account_makes_a_one_time_sign_in_link() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = admin(app, "b156-notifier").await;
        let form = get(app, "/admin/people/add", &cookies).await;
        assert_eq!(form.status, StatusCode::OK, "{}", form.body);
        assert!(
            form.body.contains("Notify user of new account"),
            "{}",
            form.body
        );
        let token = hidden(&form.body, "_token");

        let name = common::username("b156-welcomed");
        delete_user_named_on_exit(app, &name);
        let body = format!(
            "_token={token}&_form_build_id=x&name={}&mail={}&password=long-enough-pass-1&status=1&notify=1",
            enc(&name),
            enc(&format!("{name}@example.com"))
        );
        let a = post_form(
            app,
            "/admin/people/add",
            &cookies,
            body,
            &test_ip_for("b156-n"),
        )
        .await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        let id = common::user_id_of(app, &name).await;

        // The link is made after the answer; wait for it.
        let mut tokens = 0i64;
        for _ in 0..40 {
            tokens =
                sqlx::query_scalar("SELECT COUNT(*) FROM password_reset_tokens WHERE user_id = $1")
                    .bind(id)
                    .fetch_one(&app.db)
                    .await
                    .unwrap();
            if tokens > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(tokens, 1, "one one-time sign in link for the new account");
    });
}

async fn item_by(app: &TestApp, author: Uuid, status: i16) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: format!("b156 cancel {}", Uuid::now_v7().simple()),
                author_id: author,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b156".to_string()),
            },
            &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .unwrap()
        .id;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
    id
}

async fn cancel(app: &TestApp, cookies: &str, target: Uuid, method: &str) -> Answer {
    let form = get(app, &format!("/user/{target}/cancel"), cookies).await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    let token = hidden(&form.body, "_token");
    post_form(
        app,
        &format!("/user/{target}/cancel"),
        cookies,
        format!("_token={token}&method={method}"),
        &test_ip_for("b156-cancel"),
    )
    .await
}

#[test]
fn an_administrator_cancels_an_account_by_each_method() {
    run_test(async {
        let app = shared_app().await;
        let (_, admin_cookies) = admin(app, "b156-canceller").await;

        // The page offers the four methods, to an administrator of users only.
        let (target, target_cookies) = member(app, "b156-cancelled", &[]).await;
        let page = get(app, &format!("/user/{target}/cancel"), &admin_cookies).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        assert!(
            page.body
                .contains("Are you sure you want to cancel the account")
        );
        for method in ["block", "block_unpublish", "reassign", "delete"] {
            assert!(
                page.body.contains(&format!(r#"value="{method}""#)),
                "{method}"
            );
        }
        let (_, plain) = member(app, "b156-nosy", &[]).await;
        assert_eq!(
            get(app, &format!("/user/{target}/cancel"), &plain)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let nobody = get(app, &format!("/user/{target}/cancel"), "").await;
        assert_ne!(nobody.status, StatusCode::OK, "nobody is refused");
        assert!(!nobody.body.contains("Are you sure you want to cancel"));
        // One's own cancel page is the self-service deletion.
        let own = get(app, &format!("/user/{target}/cancel"), &target_cookies).await;
        assert_eq!(own.location.as_deref(), Some("/user/delete"));

        // Block and unpublish.
        let published = item_by(app, target, 1).await;
        let a = cancel(app, &admin_cookies, target, "block_unpublish").await;
        assert!(a.status.is_redirection(), "{}", a.body);
        let status: i16 = sqlx::query_scalar("SELECT status FROM users WHERE id = $1")
            .bind(target)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(status, 0, "blocked");
        let item_status: i16 = sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
            .bind(published)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(item_status, 0, "its content unpublished");

        // Block only keeps the content.
        let (keeper, _) = member(app, "b156-keeper", &[]).await;
        let kept = item_by(app, keeper, 1).await;
        cancel(app, &admin_cookies, keeper, "block").await;
        let item_status: i16 = sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
            .bind(kept)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(item_status, 1, "content kept as it was");

        // Delete and reassign to Anonymous.
        let (giver, _) = member(app, "b156-giver", &[]).await;
        let given = item_by(app, giver, 1).await;
        cancel(app, &admin_cookies, giver, "reassign").await;
        let author: Uuid = sqlx::query_scalar("SELECT author_id FROM item WHERE id = $1")
            .bind(given)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(author, Uuid::nil(), "the content belongs to Anonymous");
        assert!(app.state.users().find_by_id(giver).await.unwrap().is_none());

        // Delete with the content.
        let (leaver, _) = member(app, "b156-leaver", &[]).await;
        let gone = item_by(app, leaver, 1).await;
        cancel(app, &admin_cookies, leaver, "delete").await;
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE id = $1")
            .bind(gone)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(left, 0, "its content deleted");
        assert!(
            app.state
                .users()
                .find_by_id(leaver)
                .await
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn an_account_page_shows_the_name_and_history_to_those_allowed() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b156-profiled");
        app.create_test_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let id = common::user_id_of(app, &name).await;
        delete_user_on_exit(app, id);
        let own = app.login(&name, PASSWORD).await;
        let (_, reader) = member(app, "b156-reader", &["access user profiles"]).await;
        let (_, plain) = member(app, "b156-plain", &[]).await;

        for path in [format!("/user/{id}"), format!("/users/{name}")] {
            let a = get(app, &path, &reader).await;
            assert_eq!(a.status, StatusCode::OK, "{path}: {}", a.body);
            assert!(
                a.body
                    .contains(&format!("<h1 class=\"user-profile__name\">{name}</h1>"))
            );
            assert!(a.body.contains("Member for"), "{}", a.body);
            assert!(a.body.contains("History"), "{}", a.body);
            assert!(
                !a.body.contains("/user/profile\">Edit"),
                "no edit tab for a reader"
            );

            // Without the permission: refused, whether or not the account exists.
            assert_eq!(get(app, &path, &plain).await.status, StatusCode::FORBIDDEN);
        }
        assert_eq!(
            get(app, "/users/b156-nobody-at-all", &plain).await.status,
            StatusCode::FORBIDDEN,
            "a missing name is refused the same way"
        );
        assert_eq!(
            get(app, "/users/b156-nobody-at-all", &reader).await.status,
            StatusCode::NOT_FOUND
        );

        // My account: the owner's page at /user, with its edit tab.
        let a = get(app, "/user", &own).await;
        assert_eq!(a.status, StatusCode::OK, "{}", a.body);
        assert!(
            a.body
                .contains(&format!("<h1 class=\"user-profile__name\">{name}</h1>"))
        );
        assert!(
            a.body.contains("href=\"&#x2F;user&#x2F;profile\"")
                || a.body.contains("href=\"/user/profile\"")
        );
        let a = get(app, "/user", "").await;
        assert_eq!(a.location.as_deref(), Some("/user/login"));

        // A blocked account is no page, to anyone but its administrators.
        app.state
            .users()
            .update(
                id,
                UpdateUser {
                    status: Some(0),
                    ..Default::default()
                },
                &UserContext::administrator(Uuid::nil(), vec![]),
            )
            .await
            .unwrap();
        assert_eq!(
            get(app, &format!("/user/{id}"), &reader).await.status,
            StatusCode::NOT_FOUND
        );
        let (_, admin_cookies) = admin(app, "b156-profile-admin").await;
        assert_eq!(
            get(app, &format!("/user/{id}"), &admin_cookies)
                .await
                .status,
            StatusCode::OK
        );
    });
}
