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
use uuid::Uuid;

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
// # The empty selection tests and the selection tests
//
// The first three tests below post nothing selected. The handler still runs
// the permission check before it looks at the selection, so an empty
// selection is the narrowest request that proves the action level check
// through the real endpoint: refused without the permission, and past it (to
// "No items selected", a 303) with it.
//
// The tests after them post a real selection, one `ids[]` key per checked box
// exactly as `templates/admin/content-list.html` does, and read the items back
// from the database: what a bulk action does to the items it names, that an id
// naming no item is counted as failed rather than sinking the rest, and that a
// selection the browser did not send cleanly is refused without a write. Each
// selected item is still decided by `ItemService::update` or
// `ItemService::delete` as the acting user; what those per item gates answer
// is pinned on `ItemService` directly, in `item_service.rs`'s own tests and
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

/// The body the list screen's form sends: the token, the action, then one
/// `ids[]` pair per checked box.
fn bulk_body(token: &str, action: &str, ids: &[Uuid]) -> String {
    let mut body = format!("_token={}&action={action}", urlencoding::encode(token));
    for id in ids {
        body.push_str(&format!("&ids%5B%5D={id}"));
    }
    body
}

/// POST a bulk action selecting `ids`, scraping the screen's own CSRF token
/// first. One `ids[]` pair per id, the way the list screen's checkboxes post.
async fn bulk(
    app: &TestApp,
    cookies: &str,
    bucket: &str,
    action: &str,
    ids: &[Uuid],
) -> (StatusCode, String) {
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
                .body(Body::from(bulk_body(&token, action, ids)))
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
            let (status, body) = bulk(app, &cookies, "bulkpub-no-bucket", action, &[]).await;
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
            let (status, body) = bulk(app, &cookies, "bulkpub-yes-bucket", action, &[]).await;
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
        let (status, body) = bulk(app, &publisher, "bulkdel-no-bucket", "delete", &[]).await;
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
        let (status, body) = bulk(app, &deleter, "bulkdel-yes-bucket", "delete", &[]).await;
        assert_eq!(
            status,
            StatusCode::SEE_OTHER,
            "`delete any content` still reaches the delete arm, got {status}: {body}"
        );
    });
}

// ----------------------------------------------------------------------------
// A real selection
// ----------------------------------------------------------------------------

/// Create a `conference` item for a selection test, removed when the test ends
/// whether or not the bulk action already removed it.
async fn selectable_item(app: &TestApp, marker: &str, status: i16) -> Uuid {
    use trovato_kernel::models::CreateItem;
    use trovato_kernel::tap::UserContext;

    app.ensure_conference_type().await;
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: format!("{marker} {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: None,
                language: Some("en".to_string()),
                log: Some("bulk selection test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create a selectable item")
        .id;
    let items = std::sync::Arc::clone(app.state.items());
    common::defer_cleanup(move || async move {
        let _ = items.delete(id, &admin).await;
    });
    id
}

/// An item's published state as stored, or `None` once it is gone.
async fn stored_status(app: &TestApp, id: Uuid) -> Option<i16> {
    sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
        .bind(id)
        .fetch_optional(&app.db)
        .await
        .expect("read the item's status")
}

/// The flash the next `GET /admin/content` shows, which is where the bulk
/// action reports what it did.
async fn flash_after(app: &TestApp, cookies: &str, bucket: &str) -> String {
    let page = app
        .request_with_cookies(
            Request::get("/admin/content")
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let html = body_of(page).await;
    // The admin shell prints a message as `<div class="notice notice--KIND"
    // role=ROLE><strong ...>Label:</strong> text`, in its messages area.
    // Returned as "Label: text"; a failure must be an alert labelled Error.
    let Some(area) = html.find("class=\"admin-messages\"") else {
        return String::new();
    };
    let at = area + html[area..].find("class=\"notice notice--").unwrap();
    let tag = &html[at..at + html[at..].find('>').unwrap()];
    let label_at = at + html[at..].find("notice__label\">").unwrap() + "notice__label\">".len();
    let label = &html[label_at..label_at + html[label_at..].find('<').unwrap()];
    let start = label_at + html[label_at..].find("</strong>").unwrap() + "</strong>".len();
    let end = start + html[start..].find('<').unwrap_or(0);
    assert_eq!(
        tag.contains("role=\"alert\""),
        label == "Error:",
        "an error is an alert and nothing else is: {tag}"
    );
    format!("{label} {}", html[start..end].trim())
}

#[test]
fn bulk_publish_and_unpublish_change_exactly_the_selected_items() {
    run_test(async {
        let app = shared_app().await;
        ensure_some_content(app).await;
        let first = selectable_item(app, "bulksel-pub", 0).await;
        let second = selectable_item(app, "bulksel-pub", 0).await;
        let bystander = selectable_item(app, "bulksel-pub-bystander", 0).await;
        let (_, cookies) = user_holding(app, "bulksel-pub", &["edit any content", PUBLISH]).await;
        let bucket = "bulksel-pub-bucket";

        let (status, body) = bulk(app, &cookies, bucket, "publish", &[first, second]).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "bulk publish: {body}");
        assert_eq!(stored_status(app, first).await, Some(1));
        assert_eq!(stored_status(app, second).await, Some(1));
        assert_eq!(
            stored_status(app, bystander).await,
            Some(0),
            "an item that was not selected must not change"
        );
        assert_eq!(
            flash_after(app, &cookies, bucket).await,
            "Done: 2 items published."
        );

        let (status, body) = bulk(app, &cookies, bucket, "unpublish", &[first, second]).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "bulk unpublish: {body}");
        assert_eq!(stored_status(app, first).await, Some(0));
        assert_eq!(stored_status(app, second).await, Some(0));
        assert_eq!(
            flash_after(app, &cookies, bucket).await,
            "Done: 2 items unpublished."
        );
    });
}

#[test]
fn bulk_delete_removes_exactly_the_selected_items() {
    run_test(async {
        let app = shared_app().await;
        ensure_some_content(app).await;
        let first = selectable_item(app, "bulksel-del", 1).await;
        let second = selectable_item(app, "bulksel-del", 0).await;
        let bystander = selectable_item(app, "bulksel-del-bystander", 0).await;
        let (_, cookies) = user_holding(
            app,
            "bulksel-del",
            &["edit any content", "delete any content"],
        )
        .await;
        let bucket = "bulksel-del-bucket";

        let (status, body) = bulk(app, &cookies, bucket, "delete", &[first, second]).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "bulk delete: {body}");
        assert_eq!(stored_status(app, first).await, None, "first still exists");
        assert_eq!(
            stored_status(app, second).await,
            None,
            "second still exists"
        );
        assert_eq!(
            stored_status(app, bystander).await,
            Some(0),
            "an item that was not selected must not be deleted"
        );
        assert_eq!(
            flash_after(app, &cookies, bucket).await,
            "Done: 2 items deleted."
        );
    });
}

#[test]
fn an_id_naming_no_item_is_counted_as_failed_and_the_rest_still_happen() {
    run_test(async {
        let app = shared_app().await;
        ensure_some_content(app).await;
        let real = selectable_item(app, "bulksel-ghost", 0).await;
        let ghost = Uuid::now_v7();
        let (_, cookies) = user_holding(app, "bulksel-ghost", &["edit any content", PUBLISH]).await;
        let bucket = "bulksel-ghost-bucket";

        let (status, body) = bulk(app, &cookies, bucket, "publish", &[real, ghost]).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "bulk publish: {body}");
        assert_eq!(stored_status(app, real).await, Some(1));
        assert_eq!(
            flash_after(app, &cookies, bucket).await,
            "Error: 1 item published. Failed: 1."
        );
    });
}

#[test]
fn a_selection_with_an_unreadable_id_writes_nothing() {
    run_test(async {
        let app = shared_app().await;
        ensure_some_content(app).await;
        let real = selectable_item(app, "bulksel-bad", 0).await;
        let (_, cookies) = user_holding(app, "bulksel-bad", &["edit any content", PUBLISH]).await;
        let bucket = "bulksel-bad-bucket";

        let page = app
            .request_with_cookies(
                Request::get("/admin/content")
                    .header("x-forwarded-for", test_ip_for(bucket))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let token = token_in(&body_of(page).await).expect("the bulk form carries a token");
        let body = format!(
            "{}&ids%5B%5D=not-a-uuid",
            bulk_body(&token, "publish", &[real])
        );
        let response = app
            .request_with_cookies(
                Request::post("/admin/content/bulk")
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .header("x-forwarded-for", test_ip_for(bucket))
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            stored_status(app, real).await,
            Some(0),
            "no part of a selection with an unreadable id may be acted on"
        );
        let flash = flash_after(app, &cookies, bucket).await;
        assert!(
            flash.starts_with("Error: Nothing was changed:")
                && flash.contains("not a valid item id"),
            "the refusal is reported on the list screen, got {flash:?}"
        );
    });
}
