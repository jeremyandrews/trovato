#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Menus have no admin screen, and the documentation says so.
//!
//! `KNOWN-ISSUES.md` claimed content types, fields, users, categories, content,
//! gather queries, tiles, aliases, **menus**, plugins and AI providers "all do
//! have admin screens". There is no menu admin screen: no route under `/admin`
//! matches `menu`, and `templates/admin/` holds no menu template. Menu links are
//! rows in `menu_link`, read by the render layer and written only by config
//! import.
//!
//! A documentation defect needs a test that pins the fact, or it drifts again.
//! This one fails in both directions: if the screen is built, it says to update
//! the docs; if the docs re-acquire the claim, it says the claim is false.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, project_root, run_test, shared_app};
use uuid::Uuid;

/// The paths a menu admin screen would plausibly take, following the naming of
/// the screens that do exist (`/admin/structure/types`, `/admin/structure/aliases`).
const CANDIDATE_PATHS: [&str; 4] = [
    "/admin/structure/menus",
    "/admin/structure/menu",
    "/admin/menus",
    "/admin/config/menus",
];

async fn admin_cookies(app: &TestApp) -> String {
    let name = format!("menudoc_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

/// No menu admin screen is served.
///
/// When someone builds one, this test fails — deliberately. Add the route, then
/// move menus out of the import-only list in `KNOWN-ISSUES.md` and out of
/// "The remaining admin screens" in `ROADMAP.md`, and delete this test.
#[test]
fn no_menu_admin_screen_is_served() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app).await;

        for path in CANDIDATE_PATHS {
            let response = app
                .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), &cookies)
                .await;

            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} is served, so a menu admin screen now exists: update \
                 KNOWN-ISSUES.md and ROADMAP.md, and remove this test"
            );
        }
    });
}

/// The documentation does not claim menus have an admin screen.
#[test]
fn the_docs_do_not_claim_menus_have_an_admin_screen() {
    let known_issues = std::fs::read_to_string(project_root().join("KNOWN-ISSUES.md"))
        .expect("read KNOWN-ISSUES.md");

    // The sentence that listed what does have a screen.
    let claim_line = known_issues
        .lines()
        .find(|line| line.contains("do have admin screens"))
        .or_else(|| {
            known_issues
                .lines()
                .find(|line| line.contains("all do have admin screens"))
        });

    if let Some(line) = claim_line {
        assert!(
            !line.contains("menus"),
            "KNOWN-ISSUES.md lists menus among the types with admin screens, and \
             there is no menu admin screen: {line}"
        );
    }

    // And menus are named as import-only, so the correction is present rather
    // than the claim merely being absent.
    assert!(
        known_issues.contains("stages, menus, and system configuration"),
        "KNOWN-ISSUES.md must name menus as configuration-import only"
    );
}

/// The roadmap places the missing form before 1.0, which is what the project's own
/// criterion — a site configurable through the interface — implies.
#[test]
fn the_roadmap_places_the_menu_form_before_one_point_zero() {
    let roadmap =
        std::fs::read_to_string(project_root().join("ROADMAP.md")).expect("read ROADMAP.md");

    let before_1_0 = roadmap
        .split("## After 1.0")
        .next()
        .expect("a road to 1.0 section");

    assert!(
        before_1_0.contains("stages, menus, and system configuration"),
        "menus must be listed among the remaining admin screens before 1.0"
    );
}
