#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B267: a user create or update bumps `CacheKind::Users` exactly once, and
//! every path that creates or renames a user refuses the same names
//! `validate_username` refuses.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for, username};
use trovato_kernel::cache::generation::{CacheKind, current};
use trovato_kernel::models::{CreateUser, UpdateUser};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const BROWSER: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

struct Answer {
    status: StatusCode,
    body: String,
}

async fn answer(response: axum::response::Response) -> Answer {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Answer {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> Answer {
    let request = Request::get(path)
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", test_ip_for("b267-user-writes"))
        .body(Body::empty())
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

async fn post_form(app: &TestApp, path: &str, cookies: &str, body: String) -> Answer {
    let request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", test_ip_for("b267-user-writes"))
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

async fn admin(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = username(prefix);
    let cookies = app
        .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    delete_user_on_exit(app, id);
    (id, cookies)
}

/// Lets the background task behind [`trovato_kernel::cache::generation::bump`]
/// reach Redis before the test reads the counter back.
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

/// `UserService::create` bumps `CacheKind::Users`: a second server's cache is
/// told about the new account without a restart.
///
/// Asserts `after > before` rather than `after == before + 1`: the counter is
/// shared Redis state, and the other tests in this file (each creating or
/// logging in an admin or test user, with `RUST_TEST_THREADS=4` running all
/// four at once, as CI does) bump it too. The property worth checking is that
/// this call bumped it at all, not how many times the whole process did.
#[test]
fn create_bumps_the_users_cache_once() {
    run_test(async {
        let app = shared_app().await;
        common::settle_cache_generations().await;
        let before = current(app.state.redis(), CacheKind::Users)
            .await
            .unwrap_or(0);

        let name = username("b267-create");
        let input = CreateUser {
            name: name.clone(),
            password: "test-password-123".to_string(),
            mail: format!("{name}@example.com"),
            is_admin: false,
        };
        let user = app
            .state
            .users()
            .create(input, &UserContext::anonymous(), "test")
            .await
            .expect("create should succeed");
        delete_user_on_exit(app, user.id);

        settle().await;
        let after = current(app.state.redis(), CacheKind::Users)
            .await
            .unwrap_or(0);
        assert!(
            after > before,
            "create should bump Users: before {before}, after {after}"
        );
    });
}

/// `UserService::update` bumps `CacheKind::Users`, the same shared counter
/// [`create_bumps_the_users_cache_once`] checks; see its comment for why this
/// asserts `after > before` rather than an exact delta.
#[test]
fn update_bumps_the_users_cache_once() {
    run_test(async {
        let app = shared_app().await;
        let name = username("b267-update");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let id = common::user_id_of(app, &name).await;
        delete_user_on_exit(app, id);
        common::settle_cache_generations().await;

        let before = current(app.state.redis(), CacheKind::Users)
            .await
            .unwrap_or(0);

        let update = UpdateUser {
            mail: Some(format!("{name}-updated@example.com")),
            ..Default::default()
        };
        app.state
            .users()
            .update(id, update, &UserContext::anonymous())
            .await
            .expect("update should succeed");

        settle().await;
        let after = current(app.state.redis(), CacheKind::Users)
            .await
            .unwrap_or(0);
        assert!(
            after > before,
            "update should bump Users: before {before}, after {after}"
        );
    });
}

/// The admin "Add user" form (`/admin/people/add`) refuses a name
/// [`trovato_kernel::routes::helpers::validate_username`] refuses, with the
/// same message registration gives for the same name.
#[test]
fn admin_add_user_form_refuses_the_same_names_registration_does() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = admin(app, "b267-admin").await;

        for (bad_name, expected_message) in [
            ("a", "Username must be at least 2 characters."),
            (
                "user name",
                "Username may only contain letters, numbers, underscores, hyphens, and periods.",
            ),
        ] {
            assert_eq!(
                trovato_kernel::routes::helpers::validate_username(bad_name),
                Err(expected_message),
                "the fixture's expectation no longer matches validate_username"
            );

            let form = get(app, "/admin/people/add", &cookies).await;
            assert_eq!(form.status, StatusCode::OK, "{}", form.body);
            let token = hidden(&form.body, "_token");

            let mail = format!("{}@example.com", username("b267-badname"));
            let body = format!(
                "_token={token}&_form_build_id=x&name={}&mail={}&password=test-password-123",
                enc(bad_name),
                enc(&mail)
            );
            let a = post_form(app, "/admin/people/add", &cookies, body).await;
            assert_eq!(
                a.status,
                StatusCode::OK,
                "should re-render with an error: {}",
                a.body
            );
            assert!(
                a.body.contains(expected_message),
                "expected {expected_message:?} in body: {}",
                a.body
            );

            let still_absent: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM users WHERE mail = $1")
                    .bind(&mail)
                    .fetch_optional(&app.db)
                    .await
                    .unwrap();
            assert!(
                still_absent.is_none(),
                "no account should have been created"
            );
        }
    });
}

/// An existing, already valid-named user still saves through the admin edit
/// form: the new rename check does not refuse a name that was already fine.
#[test]
fn admin_edit_form_still_saves_an_existing_valid_user() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = admin(app, "b267-editor").await;

        let name = username("b267-valid");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let id = common::user_id_of(app, &name).await;
        delete_user_on_exit(app, id);
        delete_user_named_on_exit(app, &name);

        let form = get(app, &format!("/admin/people/{id}/edit"), &cookies).await;
        assert_eq!(form.status, StatusCode::OK, "{}", form.body);
        let token = hidden(&form.body, "_token");

        let new_mail = format!("{name}-changed@example.com");
        let body = format!(
            "_token={token}&_form_build_id=x&name={}&mail={}&status=1",
            enc(&name),
            enc(&new_mail)
        );
        let a = post_form(app, &format!("/admin/people/{id}/edit"), &cookies, body).await;
        assert!(
            a.status.is_redirection(),
            "an unchanged valid name should still save: {}: {}",
            a.status,
            a.body
        );

        let saved_mail: String = sqlx::query_scalar("SELECT mail FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(saved_mail, new_mail);
    });
}
