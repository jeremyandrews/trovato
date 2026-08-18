#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Route-level tests for what the roles screen says about consequences.
//!
//! The roles CRUD itself already existed (`integration_test.rs` covers list,
//! create, rename, delete and the built-in-role refusal). What did not exist was
//! the screen saying anything about **who is affected**: `user_roles.role_id` is
//! `ON DELETE CASCADE`, so deleting a role silently takes it away from everyone
//! holding it. That is the right behaviour and the wrong thing to discover
//! afterwards.
//!
//! These tests pin the consequence, not the CRUD: the member count is shown, the
//! confirmation says what happens, the cascade does what the screen promised, and
//! accounts survive it.
//!
//! Fixtures carry a UUID suffix and are cleaned up, so this passes under default
//! parallelism and passes again on a database it has already run against.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test, shared_app};
use uuid::Uuid;

async fn admin_session(app: &TestApp) -> String {
    let name = format!("roleadm_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

fn csrf_from(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .map(|i| i + needle.len())
        .expect("a form on this page must carry a CSRF token");
    let end = html[start..].find('"').expect("token must be terminated");
    html[start..start + end].to_string()
}

async fn get_page(app: &TestApp, cookies: &str, path: &str) -> (String, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let refreshed = extract_cookies(&response);
    let cookies = if refreshed.is_empty() {
        cookies.to_string()
    } else {
        refreshed
    };
    let body = body_text(response).await;
    assert_eq!(status, StatusCode::OK, "GET {path} should render: {body}");
    (cookies, body)
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
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

/// A role with `members` users holding it and one permission granted.
async fn seed_role(app: &TestApp, members: usize) -> (Uuid, String, Vec<Uuid>) {
    let name = format!("role_{}", Uuid::now_v7().simple());
    let role_id = Uuid::now_v7();
    sqlx::query("INSERT INTO roles (id, name) VALUES ($1, $2)")
        .bind(role_id)
        .bind(&name)
        .execute(&app.db)
        .await
        .expect("insert role");
    sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES ($1, 'access content')")
        .bind(role_id)
        .execute(&app.db)
        .await
        .expect("grant permission");

    let mut user_ids = Vec::new();
    for _ in 0..members {
        let username = format!("holder_{}", Uuid::now_v7().simple());
        app.create_test_user(
            &username,
            "test-password-123",
            &format!("{username}@example.com"),
        )
        .await;
        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&username)
            .fetch_one(&app.db)
            .await
            .expect("the user must exist");
        sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2)")
            .bind(user_id)
            .bind(role_id)
            .execute(&app.db)
            .await
            .expect("assign role");
        user_ids.push(user_id);
    }

    (role_id, name, user_ids)
}

async fn cleanup(app: &TestApp, role_id: Uuid, user_ids: &[Uuid]) {
    let _ = sqlx::query("DELETE FROM roles WHERE id = $1")
        .bind(role_id)
        .execute(&app.db)
        .await;
    for id in user_ids {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await;
    }
}

/// The listing shows how many people hold each role and how many permissions it
/// grants, and links to the grid that edits them.
#[test]
fn the_listing_shows_members_permissions_and_links_to_the_grid() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let (role_id, name, users) = seed_role(app, 2).await;

        let (_, html) = get_page(app, &cookies, "/admin/people/roles").await;

        assert!(html.contains(&name), "the role must be listed");
        assert!(
            html.contains("Members"),
            "the listing must have a members column"
        );
        assert!(
            html.contains("Permissions"),
            "the listing must have a permissions column"
        );
        assert!(
            html.contains("/admin/people/permissions"),
            "the listing must link to the permission grid"
        );

        // The row itself carries the counts: 2 members, 1 permission.
        let row_start = html.find(&name).expect("the role row");
        let row = &html[row_start..(row_start + 600).min(html.len())];
        assert!(
            row.contains(">2<"),
            "the row must show 2 members, got: {row}"
        );
        assert!(
            row.contains(">1</a>") || row.contains(">1<"),
            "the row must show 1 permission, got: {row}"
        );

        cleanup(app, role_id, &users).await;
    });
}

/// The delete confirmation says what happens to the people holding the role, and
/// what does not happen to them.
#[test]
fn the_delete_confirmation_names_the_members_it_affects() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let (role_id, name, users) = seed_role(app, 3).await;

        let (_, html) = get_page(app, &cookies, "/admin/people/roles").await;

        assert!(
            html.contains("3 user(s) hold it and will lose it"),
            "the confirmation must count the members, got: {html}"
        );
        assert!(
            html.contains("accounts are not deleted"),
            "the confirmation must say the accounts survive"
        );
        assert!(
            html.contains("Deleting a role removes it from everyone who holds it"),
            "the page must state the consequence outside the dialog too"
        );
        assert!(html.contains(&name));

        cleanup(app, role_id, &users).await;
    });
}

/// And the cascade does what the screen promised: the assignments go, the accounts
/// and every other role stay.
#[test]
fn deleting_a_role_removes_its_assignments_and_nothing_else() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let (role_id, _name, users) = seed_role(app, 2).await;

        // One of the holders also holds a second role, which must survive.
        let (other_role_id, _other_name, _) = seed_role(app, 0).await;
        sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2)")
            .bind(users[0])
            .bind(other_role_id)
            .execute(&app.db)
            .await
            .unwrap();

        let (cookies, html) = get_page(app, &cookies, "/admin/people/roles").await;
        let token = csrf_from(&html);

        let response = post_form(
            app,
            &cookies,
            &format!("/admin/people/roles/{role_id}/delete"),
            &[("_token", &token)],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let role_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles WHERE id = $1")
            .bind(role_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(role_left, 0, "the role must be gone");

        let assignments: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_roles WHERE role_id = $1")
                .bind(role_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(assignments, 0, "its assignments must be gone with it");

        let grants: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM role_permissions WHERE role_id = $1")
                .bind(role_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(grants, 0, "its permission grants must be gone with it");

        for user_id in &users {
            let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
            assert_eq!(alive, 1, "the account must survive the role's deletion");
        }

        let other_kept: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_roles WHERE role_id = $1")
                .bind(other_role_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(other_kept, 1, "another role's assignment must be untouched");

        cleanup(app, other_role_id, &users).await;
    });
}

/// The permission grid links back to the roles screen, and says what it cannot
/// grant. Both directions of the link, so neither screen is a dead end.
#[test]
fn the_permission_grid_links_back_and_states_its_limit() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        let (_, html) = get_page(app, &cookies, "/admin/people/permissions").await;

        assert!(
            html.contains("/admin/people/roles"),
            "the grid must link to the roles screen"
        );
        assert!(
            html.contains("tap_perm"),
            "the grid must say why a plugin's permissions are absent, got: {html}"
        );
    });
}

/// A non-admin reaches neither screen.
#[test]
fn a_non_admin_is_refused_both_screens() {
    run_test(async {
        let app = shared_app().await;
        let name = format!("roleusr_{}", Uuid::now_v7().simple());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;

        for path in ["/admin/people/roles", "/admin/people/permissions"] {
            let response = app
                .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), &cookies)
                .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{path} must be forbidden to a non-admin"
            );
        }
    });
}

/// A delete without a valid CSRF token leaves the role alone.
#[test]
fn a_delete_without_a_valid_csrf_token_is_rejected() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let (role_id, _name, users) = seed_role(app, 1).await;

        let response = post_form(
            app,
            &cookies,
            &format!("/admin/people/roles/{role_id}/delete"),
            &[("_token", "not-a-valid-token")],
        )
        .await;
        assert_ne!(response.status(), StatusCode::SEE_OTHER);

        let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles WHERE id = $1")
            .bind(role_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(still, 1, "the role must survive a CSRF-rejected delete");

        cleanup(app, role_id, &users).await;
    });
}
