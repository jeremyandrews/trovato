#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A user can be put in a role.
//!
//! On the 0.102.0 run there was no way to do it at all: neither the user edit
//! form nor the CLI offered role membership, so putting `netadmin` into
//! `network_admin` was done with SQL. `user_roles` has always existed and
//! `RoleService::assign_to_user` has always been there; nothing called them.
//!
//! Two paths now do: checkboxes on the user edit form, and
//! `trovato user role-add` / `role-remove` on the CLI.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::Role;
use uuid::Uuid;

async fn get(app: &TestApp, cookies: &str, path: &str, bucket: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn post(app: &TestApp, cookies: &str, path: &str, body: String, bucket: &str) -> StatusCode {
    app.request_with_cookies(
        Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", test_ip_for(bucket))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

async fn csrf_token(app: &TestApp, cookies: &str, path: &str, bucket: &str) -> String {
    let (status, html) = get(app, cookies, path, bucket).await;
    assert_eq!(status, StatusCode::OK, "cannot read a token from {path}");
    let marker = r#"name="_token" value=""#;
    let start = html.find(marker).expect("no CSRF token") + marker.len();
    let end = start + html[start..].find('"').unwrap();
    html[start..end].to_string()
}

async fn user_id_of(app: &TestApp, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(&app.db)
        .await
        .expect("user exists")
}

async fn roles_of(app: &TestApp, user_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT r.name FROM roles r JOIN user_roles ur ON r.id = ur.role_id \
         WHERE ur.user_id = $1 ORDER BY r.name",
    )
    .bind(user_id)
    .fetch_all(&app.db)
    .await
    .expect("read roles")
}

/// The body that saves a user edit form, with the given role ids ticked.
fn edit_body(token: &str, name: &str, mail: &str, role_ids: &[Uuid]) -> String {
    let mut body = format!(
        "_token={token}&_form_build_id=x&name={name}&mail={}&status=1",
        mail.replace('@', "%40")
    );
    for id in role_ids {
        body.push_str(&format!("&role_{id}=1"));
    }
    body
}

/// An administrator, a target user, and a role holding nothing special.
async fn fixture(app: &TestApp) -> (String, String, Uuid, Role, String) {
    let tag = Uuid::now_v7().simple().to_string();

    let admin = format!("rolesadmin_{tag}");
    let cookies = app
        .create_and_login_admin(&admin, "test-password-123", &format!("{admin}@example.com"))
        .await;

    let target = format!("rolestarget_{tag}");
    app.create_test_user(
        &target,
        "test-password-123",
        &format!("{target}@example.com"),
    )
    .await;
    let target_id = user_id_of(app, &target).await;

    let role = Role::create(&app.db, &format!("network_admin_{tag}"))
        .await
        .expect("create role");

    (cookies, target, target_id, role, tag)
}

/// The form offers the roles, with the ones already held ticked.
#[test]
fn the_user_edit_form_offers_role_checkboxes() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, _target, target_id, role, _tag) = fixture(app).await;

        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/people/{target_id}/edit"),
            "roleform",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!(r#"name="role_{}""#, role.id)),
            "the role must be on the form, got: {html}"
        );
        assert!(html.contains(&role.name), "and named");
    });
}

/// The headline case: the form puts a user in a role.
#[test]
fn the_user_edit_form_assigns_a_role() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, target, target_id, role, _tag) = fixture(app).await;
        let path = format!("/admin/people/{target_id}/edit");

        assert!(
            roles_of(app, target_id).await.is_empty(),
            "the target starts with no roles"
        );

        let token = csrf_token(app, &cookies, &path, "roleassign").await;
        let status = post(
            app,
            &cookies,
            &path,
            edit_body(
                &token,
                &target,
                &format!("{target}@example.com"),
                &[role.id],
            ),
            "roleassign",
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        assert_eq!(
            roles_of(app, target_id).await,
            vec![role.name.clone()],
            "the role must have been assigned"
        );
    });
}

/// And takes it away again when the box is unticked.
#[test]
fn the_user_edit_form_removes_a_role() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, target, target_id, role, _tag) = fixture(app).await;
        let path = format!("/admin/people/{target_id}/edit");

        Role::assign_to_user(&app.db, target_id, role.id)
            .await
            .expect("seed membership");
        assert_eq!(roles_of(app, target_id).await, vec![role.name.clone()]);

        let token = csrf_token(app, &cookies, &path, "roleremove").await;
        let status = post(
            app,
            &cookies,
            &path,
            edit_body(&token, &target, &format!("{target}@example.com"), &[]),
            "roleremove",
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        assert!(
            roles_of(app, target_id).await.is_empty(),
            "the role must have been removed"
        );
    });
}

/// A user without `administer users` cannot change anyone's roles.
#[test]
fn a_user_without_the_permission_cannot_assign_a_role() {
    run_test(async {
        let app = shared_app().await;
        let (admin_cookies, target, target_id, role, tag) = fixture(app).await;
        let path = format!("/admin/people/{target_id}/edit");

        // A genuine token, taken by someone who may use the screen, replayed by
        // someone who may not: the permission check is what refuses, not the
        // absence of a token.
        let token = csrf_token(app, &admin_cookies, &path, "rolenoperm-admin").await;

        let outsider = format!("rolesoutsider_{tag}");
        let cookies = app
            .create_and_login_user(
                &outsider,
                "test-password-123",
                &format!("{outsider}@example.com"),
            )
            .await;

        let status = post(
            app,
            &cookies,
            &path,
            edit_body(
                &token,
                &target,
                &format!("{target}@example.com"),
                &[role.id],
            ),
            "rolenoperm",
        )
        .await;

        assert_ne!(status, StatusCode::SEE_OTHER);
        assert!(
            roles_of(app, target_id).await.is_empty(),
            "a user without `administer users` must not assign a role"
        );
    });
}

/// Nor their own, which is the case that would be an escalation.
#[test]
fn a_user_without_the_permission_cannot_assign_themselves_a_role() {
    run_test(async {
        let app = shared_app().await;
        let (admin_cookies, _target, _target_id, role, tag) = fixture(app).await;

        let outsider = format!("rolesself_{tag}");
        let cookies = app
            .create_and_login_user(
                &outsider,
                "test-password-123",
                &format!("{outsider}@example.com"),
            )
            .await;
        let own_id = user_id_of(app, &outsider).await;
        let path = format!("/admin/people/{own_id}/edit");

        let token = csrf_token(app, &admin_cookies, &path, "roleself-admin").await;
        let status = post(
            app,
            &cookies,
            &path,
            edit_body(
                &token,
                &outsider,
                &format!("{outsider}@example.com"),
                &[role.id],
            ),
            "roleself",
        )
        .await;

        assert_ne!(status, StatusCode::SEE_OTHER);
        assert!(
            roles_of(app, own_id).await.is_empty(),
            "a user must not be able to give themselves a role"
        );
    });
}

/// A delegated user administrator cannot hand out permissions they lack.
///
/// `administer users` is grantable, and roles carry permissions, so without this
/// a delegated user administrator could assign themselves a role holding
/// `administer site` and become a site administrator by way of the screen they
/// were given to manage usernames.
#[test]
fn a_delegated_administrator_cannot_grant_a_role_beyond_their_own_permissions() {
    run_test(async {
        let app = shared_app().await;
        let tag = Uuid::now_v7().simple().to_string();

        // A powerful role the delegate does not hold.
        let powerful = Role::create(&app.db, &format!("powerful_{tag}"))
            .await
            .expect("create role");
        Role::add_permission(&app.db, powerful.id, "administer site")
            .await
            .expect("grant");

        // A delegate: a non-superuser holding only `administer users`.
        let delegate_role = Role::create(&app.db, &format!("delegate_{tag}"))
            .await
            .expect("create role");
        Role::add_permission(&app.db, delegate_role.id, "administer users")
            .await
            .expect("grant");

        let delegate = format!("delegate_{tag}");
        app.create_test_user(
            &delegate,
            "test-password-123",
            &format!("{delegate}@example.com"),
        )
        .await;
        let delegate_id = user_id_of(app, &delegate).await;
        Role::assign_to_user(&app.db, delegate_id, delegate_role.id)
            .await
            .expect("assign");
        app.state.permissions().invalidate_user(delegate_id);
        let cookies = app.login(&delegate, "test-password-123").await;

        let path = format!("/admin/people/{delegate_id}/edit");
        let token = csrf_token(app, &cookies, &path, "roledelegate").await;
        let status = post(
            app,
            &cookies,
            &path,
            edit_body(
                &token,
                &delegate,
                &format!("{delegate}@example.com"),
                &[powerful.id],
            ),
            "roledelegate",
        )
        .await;

        // The save itself succeeds — the delegate may edit users — but the role
        // they may not delegate is not applied.
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(
            !roles_of(app, delegate_id).await.contains(&powerful.name),
            "a delegate must not grant a role carrying permissions they lack, got {:?}",
            roles_of(app, delegate_id).await
        );
    });
}

/// The delegate can still hand out a role wholly within their own permissions.
///
/// The guard above must not make delegation useless.
#[test]
fn a_delegated_administrator_can_grant_a_role_within_their_permissions() {
    run_test(async {
        let app = shared_app().await;
        let tag = Uuid::now_v7().simple().to_string();

        let delegate_role = Role::create(&app.db, &format!("delegate2_{tag}"))
            .await
            .expect("create role");
        for permission in ["administer users", "access content"] {
            Role::add_permission(&app.db, delegate_role.id, permission)
                .await
                .expect("grant");
        }

        // A role holding only something the delegate also holds.
        let modest = Role::create(&app.db, &format!("modest_{tag}"))
            .await
            .expect("create role");
        Role::add_permission(&app.db, modest.id, "access content")
            .await
            .expect("grant");

        let delegate = format!("delegate2_{tag}");
        app.create_test_user(
            &delegate,
            "test-password-123",
            &format!("{delegate}@example.com"),
        )
        .await;
        let delegate_id = user_id_of(app, &delegate).await;
        Role::assign_to_user(&app.db, delegate_id, delegate_role.id)
            .await
            .expect("assign");
        app.state.permissions().invalidate_user(delegate_id);
        let cookies = app.login(&delegate, "test-password-123").await;

        let target = format!("delegate2target_{tag}");
        app.create_test_user(
            &target,
            "test-password-123",
            &format!("{target}@example.com"),
        )
        .await;
        let target_id = user_id_of(app, &target).await;

        let path = format!("/admin/people/{target_id}/edit");
        let token = csrf_token(app, &cookies, &path, "roledelegate2").await;
        post(
            app,
            &cookies,
            &path,
            edit_body(
                &token,
                &target,
                &format!("{target}@example.com"),
                &[modest.id],
            ),
            "roledelegate2",
        )
        .await;

        assert!(
            roles_of(app, target_id).await.contains(&modest.name),
            "a delegate must still be able to grant what they hold, got {:?}",
            roles_of(app, target_id).await
        );
    });
}

/// A non-superuser cannot make anyone a superuser through the form.
///
/// The `is_admin` checkbox is on the same form as the roles, and
/// `administer users` is grantable, so honouring it for a non-superuser would
/// make the permission a self-escalation.
#[test]
fn a_delegated_administrator_cannot_set_the_superuser_flag() {
    run_test(async {
        let app = shared_app().await;
        let tag = Uuid::now_v7().simple().to_string();

        let delegate_role = Role::create(&app.db, &format!("delegate3_{tag}"))
            .await
            .expect("create role");
        Role::add_permission(&app.db, delegate_role.id, "administer users")
            .await
            .expect("grant");

        let delegate = format!("delegate3_{tag}");
        app.create_test_user(
            &delegate,
            "test-password-123",
            &format!("{delegate}@example.com"),
        )
        .await;
        let delegate_id = user_id_of(app, &delegate).await;
        Role::assign_to_user(&app.db, delegate_id, delegate_role.id)
            .await
            .expect("assign");
        app.state.permissions().invalidate_user(delegate_id);
        let cookies = app.login(&delegate, "test-password-123").await;

        let path = format!("/admin/people/{delegate_id}/edit");
        let token = csrf_token(app, &cookies, &path, "rolesuper").await;
        post(
            app,
            &cookies,
            &path,
            format!(
                "_token={token}&_form_build_id=x&name={delegate}&mail={delegate}%40example.com&status=1&is_admin=1"
            ),
            "rolesuper",
        )
        .await;

        let is_admin: bool = sqlx::query_scalar("SELECT is_admin FROM users WHERE id = $1")
            .bind(delegate_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(
            !is_admin,
            "a non-superuser must not be able to make themselves one"
        );
    });
}

/// The CLI path, exercised at the layer the CLI calls.
///
/// `run_user_command` builds its own pool from the environment and is private to
/// the binary, so the test drives the same model calls its arms do:
/// `find_by_name`, `get_user_roles`, `assign_to_user`, `remove_from_user`. What
/// this pins is that the data layer the verbs sit on behaves, including the
/// idempotence the `role-add` arm reports on.
#[test]
fn the_cli_role_verbs_assign_and_remove_by_name() {
    run_test(async {
        let app = shared_app().await;
        let (_cookies, target, target_id, role, _tag) = fixture(app).await;

        // `user role-add <name> <role>`: both looked up by name.
        let user = trovato_kernel::models::User::find_by_name(&app.db, &target)
            .await
            .expect("query")
            .expect("the user must be found by name");
        let found = Role::find_by_name(&app.db, &role.name)
            .await
            .expect("query")
            .expect("the role must be found by name");

        Role::assign_to_user(&app.db, user.id, found.id)
            .await
            .expect("assign");
        assert_eq!(roles_of(app, target_id).await, vec![role.name.clone()]);

        // Re-running is not an error; the arm reports "already had".
        Role::assign_to_user(&app.db, user.id, found.id)
            .await
            .expect("assigning twice must not fail");
        assert_eq!(
            roles_of(app, target_id).await,
            vec![role.name.clone()],
            "and must not duplicate the membership"
        );

        // `user role-remove <name> <role>`.
        Role::remove_from_user(&app.db, user.id, found.id)
            .await
            .expect("remove");
        assert!(roles_of(app, target_id).await.is_empty());
    });
}
