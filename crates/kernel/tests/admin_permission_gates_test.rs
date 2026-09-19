#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Admin screens gate on a permission, not on the superuser flag.
//!
//! Every admin route in `crates/kernel/src/routes/` used to call
//! `require_admin`, which reads the `users.is_admin` column. That column cannot
//! be granted to a role or named in a `role.*.yml` file, so a role holding a
//! plugin's own permissions still could not use the admin UI to exercise them:
//! the friction log's case was a role granted `administer argus` that could not
//! open `/admin/content/add/argus_feed`.
//!
//! The conversion has three properties and this file pins all three:
//!
//! - a **non-superuser** who holds the permission gets in (the new behaviour);
//! - a user who holds no permissions is still refused (nothing was weakened);
//! - the **superuser bypass** still works on a converted route.
//!
//! The per-type content permission is tested by name, because that is the one
//! the friction log named and the one whose string is built at runtime from the
//! path rather than written as a literal.
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
    let role = Role::create(&app.db, &format!("permgate-{}", Uuid::now_v7().simple()))
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

/// Create a non-superuser holding exactly `permissions`, and log them in.
async fn user_holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String) {
    let name = username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = user_id_of(app, &name).await;
    if !permissions.is_empty() {
        grant_via_role(app, id, permissions).await;
    }
    let cookies = app.login(&name, "test-password-123").await;
    (id, cookies)
}

/// GET `path` as the holder of `cookies`, in that user's own rate-limit bucket.
async fn get_as(app: &TestApp, path: &str, cookies: &str, bucket: &str) -> StatusCode {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    response.status()
}

/// Enable the plugins whose gates sit in front of routes in [`SURFACES`].
///
/// `/admin/structure/categories` is behind `gate_categories`, so on a clean
/// database it is 404 before it is ever 403 and the permission check below is
/// never reached. A developer database usually has the plugin enabled already,
/// which is exactly why this has to be explicit rather than assumed.
async fn enable_gated_plugins(app: &TestApp) {
    app.ensure_plugin_enabled("trovato_categories").await;
}

/// A representative converted route from each permission family.
///
/// One row per distinct permission string rather than per route: the conversion
/// is mechanical and identical within a family, so a row proves the family.
const SURFACES: &[(&str, &str)] = &[
    ("/admin", "administer site"),
    ("/admin/structure/types", "administer site"),
    ("/admin/structure/menus", "administer site"),
    ("/admin/content", "edit any content"),
    ("/admin/content/add", "create content"),
    ("/admin/people", "administer users"),
    ("/admin/people/permissions", "administer users"),
    ("/admin/structure/categories", "administer categories"),
    ("/admin/content/files", "access files"),
];

#[test]
fn a_role_holding_the_permission_reaches_the_admin_screen() {
    run_test(async {
        let app = shared_app().await;
        enable_gated_plugins(app).await;

        for (path, permission) in SURFACES {
            let (_, cookies) = user_holding(app, "permgate-yes", &[permission]).await;
            let status = get_as(app, path, &cookies, path).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "a non-superuser holding `{permission}` should reach {path}, got {status}"
            );
        }
    });
}

#[test]
fn a_role_without_the_permission_is_refused() {
    run_test(async {
        let app = shared_app().await;
        enable_gated_plugins(app).await;

        // One user with no permissions at all, checked against every surface.
        let (_, cookies) = user_holding(app, "permgate-no", &[]).await;

        for (path, permission) in SURFACES {
            let status = get_as(app, path, &cookies, "permgate-no-bucket").await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "a user without `{permission}` must not reach {path}, got {status}"
            );
        }
    });
}

#[test]
fn holding_one_admin_permission_does_not_open_the_others() {
    run_test(async {
        let app = shared_app().await;
        enable_gated_plugins(app).await;

        // `administer comments` is a real permission and grants exactly its own
        // screens. If the conversion had collapsed everything onto one check,
        // this user would reach all of them.
        let (_, cookies) = user_holding(app, "permgate-narrow", &["administer comments"]).await;

        for path in ["/admin/people", "/admin/content", "/admin/structure/types"] {
            let status = get_as(app, path, &cookies, "permgate-narrow-bucket").await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "`administer comments` must not open {path}, got {status}"
            );
        }
    });
}

#[test]
fn the_superuser_bypass_still_reaches_every_converted_screen() {
    run_test(async {
        let app = shared_app().await;
        enable_gated_plugins(app).await;

        // A superuser with no roles at all: every pass below is the bypass.
        let name = username("permgate-super");
        app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;

        for (path, _) in SURFACES {
            let status = get_as(app, path, &cookies, "permgate-super-bucket").await;
            assert_eq!(
                status,
                StatusCode::OK,
                "the superuser bypass must still reach {path}, got {status}"
            );
        }
    });
}

/// The friction log's case, by name.
///
/// `/admin/content/add/{type}` builds `create {type} content` from the path.
/// Before the conversion this was `require_admin`, so a role holding exactly
/// the content permission for a type got 403 on the screen that creates it.
#[test]
fn the_per_type_content_permission_opens_the_admin_add_form() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let (_, cookies) =
            user_holding(app, "permgate-type-yes", &["create conference content"]).await;

        let status = get_as(
            app,
            "/admin/content/add/conference",
            &cookies,
            "permgate-type-yes-bucket",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "`create conference content` should open the admin add form, got {status}"
        );
    });
}

#[test]
fn the_permission_for_one_type_does_not_open_the_add_form_for_another() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        // Holds the permission for `page`, asks for `conference`. A single
        // `create content` check, or a check that ignored the path, would pass
        // this and it must not.
        let (_, cookies) = user_holding(app, "permgate-type-no", &["create page content"]).await;

        let status = get_as(
            app,
            "/admin/content/add/conference",
            &cookies,
            "permgate-type-no-bucket",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "`create page content` must not open the conference add form, got {status}"
        );
    });
}
