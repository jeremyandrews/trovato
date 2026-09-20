#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The kernel has one notion of administrator (BL-33).
//!
//! It used to have two. `require_admin` and `require_permission` read the
//! `users.is_admin` column; `UserContext::is_admin()` read the presence of an
//! `"administer site"` string in the context's permission list, and the context
//! builder pushed that string in for a column administrator. The two notions
//! disagreed in both directions, and each direction was a real defect:
//!
//! - a role granted `administer site` and nothing else passed
//!   `UserContext::is_admin()`, so it passed every bypass keyed on it —
//!   including the item routes — while still being refused by
//!   `require_permission` on the admin screens;
//! - a column administrator's context carried the marker, so a plugin asking
//!   `current-user-has-permission` got a literal answer from a list holding the
//!   marker and nothing the administrator's roles really granted.
//!
//! `administer site` is now an ordinary permission: it opens the structure and
//! configuration screens #92 gated on it, and nothing else. The column travels
//! on the context as itself and `UserContext::can` is the one bypass.
//!
//! The plugin half — an administrator passing a plugin's own
//! `current_user_has_permission` — is pinned in `ai_assistant_test.rs`, which
//! has the wasm fixture and the scripted provider to drive it.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::Role;
use uuid::Uuid;

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

/// Grant `permissions` to `user_id` through a role, the way a real site does.
async fn grant_via_role(app: &TestApp, user_id: Uuid, permissions: &[&str]) {
    let role = Role::create(&app.db, &format!("onadmin-{}", Uuid::now_v7().simple()))
        .await
        .expect("create role");
    for permission in permissions {
        Role::add_permission(&app.db, role.id, permission)
            .await
            .expect("add permission to role");
    }
    Role::assign_to_user(&app.db, user_id, role.id)
        .await
        .expect("assign role to user");
    app.state.permissions().invalidate_user(user_id);
}

/// Create a **non-superuser** holding exactly `permissions`, and log them in.
async fn user_holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> String {
    let name = username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = user_id_of(app, &name).await;
    if !permissions.is_empty() {
        grant_via_role(app, id, permissions).await;
    }
    app.login(&name, "test-password-123").await
}

/// Create a superuser (the `users.is_admin` column) with **no** roles, and log
/// them in. Every pass they get is the column.
async fn superuser(app: &TestApp, prefix: &str) -> String {
    let name = username(prefix);
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

/// GET `path` as the holder of `cookies`, in that caller's own rate-limit
/// bucket. The suite shares one bucket per source address, so a test that does
/// not name its own gets a 403 from somewhere else entirely.
async fn get_as(app: &TestApp, path: &str, cookies: &str, bucket: &str) -> StatusCode {
    app.request_with_cookies(
        Request::get(path)
            .header("x-forwarded-for", test_ip_for(bucket))
            .body(Body::empty())
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// GET `path` with no session at all.
async fn get_anonymous(app: &TestApp, path: &str, bucket: &str) -> StatusCode {
    app.request(
        Request::get(path)
            .header("x-forwarded-for", test_ip_for(bucket))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .status()
}

/// The item route that asks for a permission of its own.
///
/// `/item/add/{type}` builds `create {type} content` from the path and checks
/// it against the viewer's context — the check that `UserContext::is_admin()`
/// used to short-circuit for anyone holding `administer site`.
const ADD_CONFERENCE: &str = "/item/add/conference";

/// The defect, stated as a test.
///
/// A role holding `administer site` and nothing else was an administrator to
/// every context-based check in the kernel. It is now a role holding one
/// ordinary permission, and that permission is not `create conference content`.
#[test]
fn administer_site_alone_does_not_open_an_item_route() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let cookies = user_holding(app, "onadmin-site-only", &["administer site"]).await;
        let status = get_as(app, ADD_CONFERENCE, &cookies, "onadmin-site-only").await;

        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "`administer site` is a permission, not a superuser flag: a role \
             holding it and nothing else must not reach {ADD_CONFERENCE}, got {status}"
        );
    });
}

/// Nothing was weakened for the people the permission is for.
#[test]
fn the_named_permission_still_opens_the_item_route() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let cookies = user_holding(app, "onadmin-named", &["create conference content"]).await;
        let status = get_as(app, ADD_CONFERENCE, &cookies, "onadmin-named").await;

        assert_eq!(
            status,
            StatusCode::OK,
            "a non-administrator holding `create conference content` must reach \
             {ADD_CONFERENCE}, got {status}"
        );
    });
}

/// The column is still the one bypass, on the route the permission does not name.
#[test]
fn the_superuser_column_still_opens_the_item_route() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let cookies = superuser(app, "onadmin-super-item").await;
        let status = get_as(app, ADD_CONFERENCE, &cookies, "onadmin-super-item").await;

        assert_eq!(
            status,
            StatusCode::OK,
            "the superuser column must still reach {ADD_CONFERENCE} with no role \
             granting it, got {status}"
        );
    });
}

/// What `administer site` is actually for, after #92: the structure and
/// configuration screens. Losing the item routes must not cost it these.
#[test]
fn administer_site_still_opens_the_structure_and_configuration_screens() {
    run_test(async {
        let app = shared_app().await;

        let cookies = user_holding(app, "onadmin-structure", &["administer site"]).await;

        for path in [
            "/admin",
            "/admin/structure/types",
            "/admin/structure/menus",
        ] {
            let status = get_as(app, path, &cookies, "onadmin-structure").await;
            assert_eq!(
                status,
                StatusCode::OK,
                "`administer site` must still open {path}, got {status}"
            );
        }
    });
}

/// The anonymous role is untouched: it never held the marker, so it can neither
/// gain nor lose anything here.
#[test]
fn the_anonymous_role_is_unaffected() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        // Still served the public page its role grants.
        let front = get_anonymous(app, "/", "onadmin-anon-front").await;
        assert_eq!(
            front,
            StatusCode::OK,
            "the anonymous role still grants the front page, got {front}"
        );

        // And still refused the route it has no permission for.
        let add = get_anonymous(app, ADD_CONFERENCE, "onadmin-anon-add").await;
        assert_eq!(
            add,
            StatusCode::FORBIDDEN,
            "anonymous must not reach {ADD_CONFERENCE}, got {add}"
        );

        // An anonymous context is not an administrator, whatever its role holds.
        let status = get_anonymous(app, "/admin/structure/types", "onadmin-anon-admin").await;
        assert_ne!(
            status,
            StatusCode::OK,
            "anonymous must not open the structure screens, got {status}"
        );
    });
}
