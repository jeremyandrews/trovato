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
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding, username};

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
    // The dashboard is admission to the section rather than a screen that does
    // work, so since #97 it takes `access administration pages`. Every row below
    // it still names the permission its own screen requires.
    ("/admin", "access administration pages"),
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

// ============================================================================
// S4 — bulk publish and unpublish need `publish content` too.
//
// The endpoint is gated on the permission for the action it performs, which is
// why bulk delete asks for a delete. Publishing was the one action whose
// authority did not exist: `edit any content` was enough to put every selected
// item on the live site. Asked once, here, so the screen refuses rather than
// reporting every item as failed.
//
// # Why these post an empty selection
//
// `POST /admin/content/bulk` cannot parse a selection **at all**, on `main` and
// before it: `BulkActionForm::ids` is a `Vec<Uuid>` read from the repeated
// `ids[]` key, and `axum::Form` deserializes with `serde_urlencoded`, which
// refuses a sequence — `Failed to deserialize form body: ids[]: invalid type:
// string "…", expected a sequence`, a 422 before the handler runs. Every bulk
// action a browser has ever submitted with something selected has failed that
// way. It is a pre-existing defect with its own fix (and its own changelog
// entry) to write, and it is not this change's.
//
// An empty selection does parse (`#[serde(default)]`), so the handler runs, the
// permission check happens, and the action then reports "No items selected".
// That is exactly the check this change added, tested through the real
// endpoint. What each selected item's write would do is pinned on
// `ItemService::gate_publish` directly, in `item_service.rs`'s own tests and
// over HTTP in `item_form_roundtrip_test.rs`.
// ============================================================================

const PUBLISH: &str = "publish content";

/// Make sure `/admin/content` has at least one row.
///
/// The bulk form, and so the CSRF token these tests scrape, is inside
/// `{% if items %}`: on a database with no content the screen renders no form
/// at all and there is nothing to post.
async fn ensure_some_content(app: &TestApp) {
    app.ensure_conference_items().await;
}

/// Scrape a `_token` out of a rendered admin page.
fn token_in(html: &str) -> Option<String> {
    let at = html.find(r#"name="_token""#)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// POST a bulk action with nothing selected, scraping the screen's own CSRF
/// token first. See this section's header for why nothing is selected.
async fn bulk(app: &TestApp, cookies: &str, bucket: &str, action: &str) -> (StatusCode, String) {
    let page = app
        .request_with_cookies(
            Request::get("/admin/content")
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let token = token_in(&body_of(page).await).expect("the bulk form carries a token");

    let response = app
        .request_with_cookies(
            Request::post("/admin/content/bulk")
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::from(format!(
                    "_token={}&action={action}",
                    urlencoding::encode(&token)
                )))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

#[test]
fn bulk_publish_and_unpublish_are_refused_without_the_publish_permission() {
    run_test(async {
        let app = shared_app().await;
        ensure_some_content(app).await;
        let (_, cookies) = user_holding(app, "bulkpub-no", &["edit any content"]).await;

        for action in ["publish", "unpublish"] {
            let (status, body) = bulk(app, &cookies, "bulkpub-no-bucket", action).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "bulk {action} without `{PUBLISH}` must be refused, got {status}: {body}"
            );
        }
    });
}

#[test]
fn bulk_publish_passes_the_permission_check_with_both_permissions() {
    run_test(async {
        let app = shared_app().await;
        ensure_some_content(app).await;
        let (_, cookies) = user_holding(app, "bulkpub-yes", &["edit any content", PUBLISH]).await;

        for action in ["publish", "unpublish"] {
            let (status, body) = bulk(app, &cookies, "bulkpub-yes-bucket", action).await;
            assert_eq!(
                status,
                StatusCode::SEE_OTHER,
                "a holder of both permissions reaches the handler, got {status}: {body}"
            );
        }
    });
}

#[test]
fn bulk_delete_still_asks_for_the_delete_permission_and_not_for_publish() {
    run_test(async {
        let app = shared_app().await;
        ensure_some_content(app).await;

        // The arms did not collapse into one check: a role that may publish
        // still cannot delete, and a role that may delete needs no publish
        // permission. (`edit any content` is here only to open
        // `/admin/content`, which is where the screen's CSRF token comes from.)
        let (_, publisher) = user_holding(app, "bulkdel-no", &["edit any content", PUBLISH]).await;
        let (status, body) = bulk(app, &publisher, "bulkdel-no-bucket", "delete").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "`{PUBLISH}` must not buy a delete, got {status}: {body}"
        );

        let (_, deleter) = user_holding(
            app,
            "bulkdel-yes",
            &["edit any content", "delete any content"],
        )
        .await;
        let (status, body) = bulk(app, &deleter, "bulkdel-yes-bucket", "delete").await;
        assert_eq!(
            status,
            StatusCode::SEE_OTHER,
            "`delete any content` still reaches the delete arm, got {status}: {body}"
        );
    });
}
