#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `access administration pages`: admission to the section, and nothing more.
//!
//! #92 gave every admin screen its own permission, and left `/admin` itself on
//! `administer site`. That was a gap with a shape: a role delegated
//! `administer comments` could reach `/admin/content/comments` by typing the
//! address, and got 403 on the dashboard that would have linked to it. The
//! delegation worked everywhere except at the front door.
//!
//! The permission that closes it is deliberately the weakest one in the kernel.
//! It opens `/admin` and confers no authority inside it: every screen the
//! dashboard links to still asks for its own permission, and the dashboard's
//! own cards are filtered to what the viewer may actually open, so the page
//! never offers a door that answers 403.
//!
//! **The admin layout's sidebar is not filtered**, and is deliberately left
//! alone here. It has listed every screen unconditionally since #92 made those
//! screens delegable, so it is the same for any delegated role reaching any
//! admin page and is not something this permission introduces. Filtering it
//! means giving `render_admin_template` the viewer, which is a signature change
//! across its 80 call sites and belongs in its own change. The assertions below
//! are therefore scoped to the dashboard's own content region.
//!
//! It implies nothing and nothing implies it. The one exception is a migration
//! that grants it once to every role already holding `administer site`, so no
//! existing site loses its dashboard on upgrade;
//! [`a_role_holding_administer_site_still_reaches_the_dashboard`] is that
//! grant's test.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::Role;
use uuid::Uuid;

const ADMISSION: &str = "access administration pages";

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
    let role = Role::create(&app.db, &format!("admission-{}", Uuid::now_v7().simple()))
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

/// GET `path` as the holder of `cookies`, returning status and body.
///
/// Each caller names its own rate-limit bucket: the suite shares one per source
/// address, so a test that does not gets a 403 from somewhere else entirely.
async fn get_as(app: &TestApp, path: &str, cookies: &str, bucket: &str) -> (StatusCode, String) {
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
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    (status, body)
}

/// The dashboard's own content, without the layout's sidebar.
///
/// The sidebar lists every admin screen unconditionally and has done since #92
/// (see this file's header), so a whole-page assertion would be testing that
/// pre-existing behaviour rather than this change. These tests are about what
/// the dashboard itself offers.
fn dashboard_main(body: &str) -> &str {
    // The opening tag, not the stylesheet rule of the same name that precedes
    // the sidebar: anchoring on the bare string would slice from the `<style>`
    // block and take the sidebar with it.
    const MAIN: &str = "<main class=\"admin-content\">";
    let start = body.find(MAIN).map(|i| i + MAIN.len()).unwrap_or(0);
    let end = body[start..]
        .find("</main>")
        .map(|i| start + i)
        .unwrap_or(body.len());
    &body[start..end]
}

/// The gap, closed: a delegated moderator reaches the dashboard.
#[test]
fn a_delegated_role_with_the_permission_reaches_the_dashboard() {
    run_test(async {
        let app = shared_app().await;

        let cookies = user_holding(app, "admission-yes", &["administer comments", ADMISSION]).await;
        let (status, _) = get_as(app, "/admin", &cookies, "admission-yes").await;

        assert_eq!(
            status,
            StatusCode::OK,
            "`administer comments` plus `{ADMISSION}` must open /admin, got {status}"
        );
    });
}

/// And is shown only what it may open.
///
/// The same role, on the same page: no structure card, because
/// `/admin/structure/types` takes `administer site` and this role has not got
/// it. A dashboard that linked there would be offering a door that answers 403.
#[test]
fn the_dashboard_shows_only_the_links_the_viewer_may_open() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let cookies =
            user_holding(app, "admission-links", &["administer comments", ADMISSION]).await;
        let (status, body) = get_as(app, "/admin", &cookies, "admission-links").await;
        assert_eq!(status, StatusCode::OK, "the dashboard must render");
        let main = dashboard_main(&body);

        assert!(
            !main.contains("/admin/structure/types"),
            "a role without `administer site` must not be shown the structure \
             link: {main}"
        );
        assert!(
            !main.contains("/item/add/conference"),
            "a role without `create conference content` must not be shown the \
             link that creates one: {main}"
        );
    });
}

/// A viewer who may create a type is shown that link and no other.
#[test]
fn a_creatable_type_is_linked_and_an_uncreatable_one_is_not() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let cookies = user_holding(
            app,
            "admission-create",
            &["create conference content", ADMISSION],
        )
        .await;
        let (status, body) = get_as(app, "/admin", &cookies, "admission-create").await;
        assert_eq!(status, StatusCode::OK, "the dashboard must render");
        let main = dashboard_main(&body);

        assert!(
            main.contains("/item/add/conference"),
            "a role holding `create conference content` must be shown that \
             link: {main}"
        );
        assert!(
            !main.contains("/admin/structure/types"),
            "and still not the structure link it may not open: {main}"
        );
    });
}

/// Nothing was given away: the permission is required, not optional.
#[test]
fn the_same_role_without_the_permission_is_still_refused() {
    run_test(async {
        let app = shared_app().await;

        let cookies = user_holding(app, "admission-no", &["administer comments"]).await;
        let (status, _) = get_as(app, "/admin", &cookies, "admission-no").await;

        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "`administer comments` alone must not open /admin, got {status}"
        );
    });
}

/// The migration's grant, observed through the door it exists to keep open.
///
/// A role holding `administer site` was granted `access administration pages`
/// by the migration, so a site that upgrades does not lose its dashboard. This
/// asserts the grant is in the database as well as the effect, because the
/// effect alone would also be produced by `administer site` implying admission,
/// which it must not.
#[test]
fn a_role_holding_administer_site_still_reaches_the_dashboard() {
    run_test(async {
        let app = shared_app().await;

        let granted: bool = sqlx::query_scalar(
            "SELECT EXISTS (\
               SELECT 1 FROM role_permissions a \
               JOIN role_permissions b ON a.role_id = b.role_id \
               WHERE a.permission = 'administer site' AND b.permission = $1)",
        )
        .bind(ADMISSION)
        .fetch_one(&app.db)
        .await
        .expect("query the grant");
        assert!(
            granted,
            "the migration must have granted `{ADMISSION}` to the roles holding \
             `administer site`"
        );

        // A role configured the way a pre-upgrade site's was: `administer site`
        // plus the permission the migration would have given it.
        let cookies =
            user_holding(app, "admission-migrated", &["administer site", ADMISSION]).await;
        let (status, _) = get_as(app, "/admin", &cookies, "admission-migrated").await;

        assert_eq!(
            status,
            StatusCode::OK,
            "an upgraded site's administrator role must keep the dashboard, got {status}"
        );
    });
}

/// `administer site` does not imply admission on its own.
///
/// The migration is a one-time correction, not a rule. A role granted
/// `administer site` *after* the upgrade — as this one is, with no admission
/// permission — does not get into the section for free.
#[test]
fn administer_site_alone_does_not_imply_admission() {
    run_test(async {
        let app = shared_app().await;

        let cookies = user_holding(app, "admission-implies", &["administer site"]).await;
        let (status, _) = get_as(app, "/admin", &cookies, "admission-implies").await;

        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "`administer site` must not imply `{ADMISSION}`, got {status}"
        );
    });
}

/// And admission implies nothing inside the section.
#[test]
fn admission_alone_opens_no_screen_inside_the_section() {
    run_test(async {
        let app = shared_app().await;

        let cookies = user_holding(app, "admission-only", &[ADMISSION]).await;

        for path in [
            "/admin/structure/types",
            "/admin/structure/menus",
            "/admin/people",
            "/admin/content",
        ] {
            let (status, _) = get_as(app, path, &cookies, "admission-only").await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "`{ADMISSION}` is admission, not authority: it must not open \
                 {path}, got {status}"
            );
        }
    });
}

/// The superuser is unaffected: the column is still the one bypass.
#[test]
fn a_superuser_is_unaffected() {
    run_test(async {
        let app = shared_app().await;

        let name = username("admission-super");
        app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;

        let (status, _) = get_as(app, "/admin", &cookies, "admission-super").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a superuser holding no roles at all must still open /admin, got {status}"
        );
    });
}
