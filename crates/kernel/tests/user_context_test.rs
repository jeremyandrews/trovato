#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Request-scoped `UserContext`s carry the viewer's **real** permissions.
//!
//! The front-page bug was a handler that built a `UserContext` from a hard-coded
//! permission list, which silently broke the item front page for every
//! logged-out visitor. This file pins the two halves of the fix that need a
//! database to observe:
//!
//! - An admin's context is **loaded**, not fabricated. It carries the
//!   permissions the admin's roles actually grant, alongside the
//!   `"administer site"` marker. The old `admin_user_context` returned
//!   `vec!["administer site"]` and nothing else, and got away with it only
//!   because `is_admin()` short-circuits every permission check.
//! - The self-service routes (profile update, password change) still work for
//!   the owner. Those service methods authorize nothing themselves — the route
//!   gates on identity or on a verified token — so this is the test that would
//!   catch a permission check added there later starting to deny real users.
//!
//! The per-viewer navigation filter is unit-tested in `menu::registry`
//! (`root_menus_for`): the menu registry is built once, at `AppState`
//! construction, from the plugins enabled at that moment, so a test cannot put a
//! permission-gated entry into a running app's navigation.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::Role;
use trovato_kernel::routes::helpers::{admin_user_context, user_context_for};
use uuid::Uuid;

/// A permission no other test grants, so seeing it proves it was loaded from
/// this user's role rather than inherited from a literal list somewhere.
const GRANTED: &str = "user context test:granted";

/// Unique username, so parallel test binaries never share a user, a rate-limit
/// bucket, or a password.
fn username(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

async fn user_id_of(app: &TestApp, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(&app.db)
        .await
        .expect("test user should exist")
}

/// Grant `permission` to `user_id` through a role, the way a real site does.
async fn grant_via_role(app: &TestApp, user_id: Uuid, permission: &str) {
    let role = Role::create(&app.db, &format!("uctx-{}", Uuid::now_v7().simple()))
        .await
        .expect("create role");
    Role::add_permission(&app.db, role.id, permission)
        .await
        .expect("add permission to role");
    Role::assign_to_user(&app.db, user_id, role.id)
        .await
        .expect("assign role to user");
    app.state.permissions().invalidate_user(user_id);
}

async fn load_user(app: &TestApp, user_id: Uuid) -> trovato_kernel::models::User {
    app.state
        .users()
        .find_by_id(user_id)
        .await
        .expect("load user")
        .expect("user exists")
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

/// Fetch a CSRF token from a rendered page.
///
/// Tokens are pooled per session and single-use, so any token the page carries
/// verifies against any form in that session — but each POST needs its own GET.
async fn csrf_token(app: &TestApp, path: &str, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let body = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);

    let marker = r#"name="_token" value=""#;
    let start = html
        .find(marker)
        .map(|p| p + marker.len())
        .unwrap_or_else(|| panic!("no CSRF token on {path}"));
    let end = start + html[start..].find('"').expect("unterminated token value");
    html[start..end].to_string()
}

#[test]
fn an_admins_context_carries_their_real_permissions_and_the_admin_marker() {
    run_test(async {
        let app = shared_app().await;
        let name = username("uctx-admin");
        app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let id = user_id_of(app, &name).await;
        grant_via_role(app, id, GRANTED).await;

        let ctx = admin_user_context(&app.state, &load_user(app, id).await).await;

        // The marker is what `is_admin()` reads, and it has to be there.
        assert!(ctx.is_admin(), "admin context must satisfy is_admin()");
        // But it must not be the *whole* permission set. This is the assertion
        // the fabricated `vec!["administer site"]` failed: an admin holds their
        // real permissions too, and a permission check that stops
        // short-circuiting on `is_admin` has to find them.
        assert!(
            ctx.has_permission(GRANTED),
            "admin context dropped a permission the admin's role grants: {:?}",
            ctx.permissions
        );
        assert!(ctx.authenticated);
        assert_eq!(ctx.id, id);
    });
}

#[test]
fn a_non_admin_context_carries_role_permissions_without_the_admin_marker() {
    run_test(async {
        let app = shared_app().await;
        let name = username("uctx-user");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let id = user_id_of(app, &name).await;
        grant_via_role(app, id, GRANTED).await;

        let ctx = user_context_for(&app.state, &load_user(app, id).await).await;

        assert!(ctx.has_permission(GRANTED));
        assert!(
            !ctx.is_admin(),
            "a non-admin must not gain the admin marker"
        );
        assert!(
            !ctx.is_background(),
            "a web context is never the background principal"
        );
    });
}

#[test]
fn a_self_service_profile_update_succeeds_for_the_owner() {
    run_test(async {
        let app = shared_app().await;
        let name = username("uctx-profile");
        let mail = format!("{name}@example.com");
        // No roles, no permissions: this user holds nothing beyond the
        // authenticated role. Editing their own profile is authorized by
        // identity, and this test fails if that ever becomes a permission check.
        let cookies = app
            .create_and_login_user(&name, "test-password-123", &mail)
            .await;

        let token = csrf_token(app, "/user/profile", &cookies).await;
        let body = format!(
            "_token={token}&name={name}&mail={mail}&timezone=Europe/Rome&current_password="
        );
        let response = app
            .request_with_cookies(
                Request::post("/user/profile")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;

        // Both the accepted and the refused paths re-render the profile page
        // with 200, so the status alone proves nothing — check what it says and
        // then check that the write actually landed.
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Profile updated successfully."),
            "the owner's own profile update was refused"
        );

        let timezone: Option<String> =
            sqlx::query_scalar("SELECT timezone FROM users WHERE id = $1")
                .bind(user_id_of(app, &name).await)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(timezone.as_deref(), Some("Europe/Rome"));
    });
}

#[test]
fn a_self_service_password_change_succeeds_for_the_owner() {
    run_test(async {
        let app = shared_app().await;
        let name = username("uctx-password");
        let old_password = "test-password-123";
        let new_password = "test-password-456";
        let cookies = app
            .create_and_login_user(&name, old_password, &format!("{name}@example.com"))
            .await;

        let token = csrf_token(app, "/user/profile", &cookies).await;
        let body = format!(
            "_token={token}&current_password={old_password}&new_password={new_password}\
             &confirm_password={new_password}"
        );
        let response = app
            .request_with_cookies(
                Request::post("/user/password")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Password changed successfully."),
            "the owner's own password change was refused"
        );

        // And the write landed: the new password authenticates. `login` panics
        // on a non-200, so returning at all is the proof.
        app.login(&name, new_password).await;
    });
}
