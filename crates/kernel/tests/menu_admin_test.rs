#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Route-level tests for the menu administration screens.
//!
//! Until these routes existed, a site's navigation was editable by exactly one
//! path: hand-writing YAML and running `trovato config import`. `KNOWN-ISSUES.md`
//! and `ROADMAP.md` both said the form belonged before 1.0, because "a site can
//! be configured through the interface" is what 1.0 means here.
//!
//! Everything below goes through a real request against the real router, with a
//! real session and a real CSRF token, because that is where the behaviour under
//! test lives — `require_admin`, `require_csrf`, form decoding and the redirect
//! are all route-layer concerns that a unit test on a helper cannot reach. The
//! tree-shaping and path-validation helpers have their own unit tests beside the
//! code in `routes/admin_menu.rs`.
//!
//! Fixtures are per-test: every menu name and path carries a fresh UUID suffix,
//! so these tests pass under default parallelism and pass again on a database
//! they have already run against.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test, shared_app};
use uuid::Uuid;

// =============================================================================
// Helpers
// =============================================================================

/// Log in a fresh admin and return the session cookies.
async fn admin_session(app: &TestApp) -> String {
    let name = format!("menuadm_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

/// Log in a fresh non-admin and return the session cookies.
async fn plain_session(app: &TestApp) -> String {
    let name = format!("menuusr_{}", Uuid::now_v7().simple());
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Pull the CSRF token out of a rendered form.
fn csrf_from(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .map(|i| i + needle.len())
        .expect("a form on this page must carry a CSRF token");
    let end = html[start..].find('"').expect("token must be terminated");
    html[start..start + end].to_string()
}

/// GET a page and return (cookies, body), carrying any refreshed session cookie.
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
    // The body carries the reason on a 500 (a template error is rendered into
    // it), so it belongs in the failure message rather than being discarded.
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {path} should render, got {status}: {body}"
    );
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

/// POST a form-urlencoded body.
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

/// Create a link through the form and return its id, read back from the database.
async fn create_link(
    app: &TestApp,
    cookies: &str,
    menu: &str,
    title: &str,
    path: &str,
    parent: Option<Uuid>,
    weight: i32,
    hidden: bool,
) -> Uuid {
    let (cookies, form) =
        get_page(app, cookies, &format!("/admin/structure/menus/{menu}/add")).await;
    let token = csrf_from(&form);
    let parent_field = parent.map(|p| p.to_string()).unwrap_or_default();
    let weight_field = weight.to_string();
    let mut fields = vec![
        ("_token", token.as_str()),
        ("title", title),
        ("path", path),
        ("parent_id", parent_field.as_str()),
        ("weight", weight_field.as_str()),
    ];
    if hidden {
        fields.push(("hidden", "1"));
    }
    let response = post_form(
        app,
        &cookies,
        &format!("/admin/structure/menus/{menu}/add"),
        &fields,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "creating {title} should redirect"
    );

    sqlx::query_scalar("SELECT id FROM menu_link WHERE menu_name = $1 AND path = $2")
        .bind(menu)
        .bind(path)
        .fetch_one(&app.db)
        .await
        .expect("the created link must be a row")
}

/// A menu name unique to one test.
fn fresh_menu() -> String {
    format!("m{}", Uuid::now_v7().simple())
}

// =============================================================================
// Access control
// =============================================================================

/// Every screen requires an admin. A non-admin is refused, not redirected to a
/// login it already has.
#[test]
fn a_non_admin_is_refused_every_menu_screen() {
    run_test(async {
        let app = shared_app().await;
        let cookies = plain_session(app).await;
        let menu = fresh_menu();

        for path in [
            "/admin/structure/menus".to_string(),
            format!("/admin/structure/menus/{menu}"),
            format!("/admin/structure/menus/{menu}/add"),
        ] {
            let response = app
                .request_with_cookies(Request::get(&path).body(Body::empty()).unwrap(), &cookies)
                .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{path} must be forbidden to a non-admin"
            );
        }
    });
}

/// An anonymous visitor is sent to log in rather than shown the screen.
#[test]
fn an_anonymous_visitor_is_redirected_to_login() {
    run_test(async {
        let app = shared_app().await;
        let response = app
            .request(
                Request::get("/admin/structure/menus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            location.contains("/user/login"),
            "expected a login redirect, got {location}"
        );
    });
}

/// A write without a valid CSRF token is rejected, and writes nothing.
#[test]
fn a_write_without_a_valid_csrf_token_is_rejected() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();
        let path = format!("/csrf-{}", Uuid::now_v7().simple());

        let response = post_form(
            app,
            &cookies,
            &format!("/admin/structure/menus/{menu}/add"),
            &[
                ("_token", "not-a-valid-token"),
                ("title", "Should not exist"),
                ("path", &path),
                ("parent_id", ""),
                ("weight", "0"),
            ],
        )
        .await;

        assert_ne!(
            response.status(),
            StatusCode::SEE_OTHER,
            "a rejected write must not redirect as if it succeeded"
        );

        let landed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link WHERE path = $1")
            .bind(&path)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(landed, 0, "a CSRF-rejected write must write nothing");
    });
}

// =============================================================================
// Create, edit, reorder
// =============================================================================

/// The create path: a link with a parent, a weight and a hidden flag lands with
/// all four, and the tree lists it nested under its parent.
#[test]
fn a_link_is_created_with_its_parent_weight_and_visibility() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();

        let root = create_link(
            app,
            &cookies,
            &menu,
            "Docs",
            &format!("/{menu}/docs"),
            None,
            0,
            false,
        )
        .await;
        let child = create_link(
            app,
            &cookies,
            &menu,
            "Guide",
            &format!("/{menu}/docs/guide"),
            Some(root),
            5,
            true,
        )
        .await;

        let (parent_id, weight, hidden, plugin): (Option<Uuid>, i32, bool, String) =
            sqlx::query_as("SELECT parent_id, weight, hidden, plugin FROM menu_link WHERE id = $1")
                .bind(child)
                .fetch_one(&app.db)
                .await
                .unwrap();

        assert_eq!(parent_id, Some(root), "the parent select must be applied");
        assert_eq!(weight, 5);
        assert!(hidden, "the hidden checkbox must be applied");
        assert_eq!(plugin, "core", "a link this form creates is core-owned");

        // And the listing renders it as a child rather than a sibling.
        let (_, html) = get_page(app, &cookies, &format!("/admin/structure/menus/{menu}")).await;
        let docs_at = html.find("Docs").expect("the root must be listed");
        let guide_at = html.find("Guide").expect("the child must be listed");
        assert!(
            docs_at < guide_at,
            "the tree must render parent before child"
        );
        assert!(
            html.contains("&#8735;"),
            "a nested row must carry the indent marker"
        );
    });
}

/// Editing a link's parent moves it, including back to the top level.
#[test]
fn editing_a_link_changes_its_parent_in_both_directions() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();

        let root = create_link(
            app,
            &cookies,
            &menu,
            "Root",
            &format!("/{menu}/root"),
            None,
            0,
            false,
        )
        .await;
        let mover = create_link(
            app,
            &cookies,
            &menu,
            "Mover",
            &format!("/{menu}/mover"),
            None,
            0,
            false,
        )
        .await;

        let edit = format!("/admin/structure/menus/{menu}/{mover}/edit");

        // Top level -> under root.
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let token = csrf_from(&form);
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("title", "Mover"),
                ("path", &format!("/{menu}/mover")),
                ("parent_id", &root.to_string()),
                ("weight", "3"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let parent: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_id FROM menu_link WHERE id = $1")
                .bind(mover)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(parent, Some(root), "the link should have moved under root");

        // Under root -> back to top level, by submitting an empty parent.
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let token = csrf_from(&form);
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("title", "Mover"),
                ("path", &format!("/{menu}/mover")),
                ("parent_id", ""),
                ("weight", "3"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let parent: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_id FROM menu_link WHERE id = $1")
                .bind(mover)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(
            parent, None,
            "an empty parent field must move the link back to the top level"
        );
    });
}

/// Weight is the reorder mechanism, and the listing reflects it.
#[test]
fn changing_a_weight_reorders_siblings_in_the_listing() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();

        let first = create_link(
            app,
            &cookies,
            &menu,
            "Alpha",
            &format!("/{menu}/alpha"),
            None,
            0,
            false,
        )
        .await;
        create_link(
            app,
            &cookies,
            &menu,
            "Beta",
            &format!("/{menu}/beta"),
            None,
            5,
            false,
        )
        .await;

        let (cookies, html) =
            get_page(app, &cookies, &format!("/admin/structure/menus/{menu}")).await;
        assert!(
            html.find("Alpha").unwrap() < html.find("Beta").unwrap(),
            "Alpha (weight 0) should sort before Beta (weight 5)"
        );

        // Push Alpha below Beta.
        let edit = format!("/admin/structure/menus/{menu}/{first}/edit");
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let token = csrf_from(&form);
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("title", "Alpha"),
                ("path", &format!("/{menu}/alpha")),
                ("parent_id", ""),
                ("weight", "10"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let (_, html) = get_page(app, &cookies, &format!("/admin/structure/menus/{menu}")).await;
        assert!(
            html.find("Beta").unwrap() < html.find("Alpha").unwrap(),
            "after the reweight, Beta should sort first"
        );
    });
}

// =============================================================================
// Validation
// =============================================================================

/// A cycle is refused, and the link keeps the parent it had.
#[test]
fn a_link_cannot_be_made_its_own_descendant() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();

        let root = create_link(
            app,
            &cookies,
            &menu,
            "Root",
            &format!("/{menu}/root"),
            None,
            0,
            false,
        )
        .await;
        let child = create_link(
            app,
            &cookies,
            &menu,
            "Child",
            &format!("/{menu}/child"),
            Some(root),
            0,
            false,
        )
        .await;
        let grandchild = create_link(
            app,
            &cookies,
            &menu,
            "Grandchild",
            &format!("/{menu}/gc"),
            Some(child),
            0,
            false,
        )
        .await;

        // Try to make the root a child of its own grandchild.
        let edit = format!("/admin/structure/menus/{menu}/{root}/edit");
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let token = csrf_from(&form);
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("title", "Root"),
                ("path", &format!("/{menu}/root")),
                ("parent_id", &grandchild.to_string()),
                ("weight", "0"),
            ],
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a rejected edit re-renders the form rather than redirecting"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("cannot be its own ancestor"),
            "the form must say why, got: {html}"
        );

        let parent: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_id FROM menu_link WHERE id = $1")
                .bind(root)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(parent, None, "the refused edit must not have been applied");
    });
}

/// The parent select never offers a link its own descendants, so the cycle above
/// is unreachable through the interface as well as refused by it.
#[test]
fn the_parent_select_omits_the_links_own_subtree() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();

        let root = create_link(
            app,
            &cookies,
            &menu,
            "Root",
            &format!("/{menu}/root"),
            None,
            0,
            false,
        )
        .await;
        let child = create_link(
            app,
            &cookies,
            &menu,
            "Child",
            &format!("/{menu}/child"),
            Some(root),
            0,
            false,
        )
        .await;

        let (_, html) = get_page(
            app,
            &cookies,
            &format!("/admin/structure/menus/{menu}/{root}/edit"),
        )
        .await;

        assert!(
            !html.contains(&child.to_string()),
            "the root's own child must not be offered as its parent"
        );
        assert!(
            html.contains("&lt;Top level&gt;"),
            "the top-level option must be offered"
        );
    });
}

/// Malformed input is rejected with a message, and nothing is written.
#[test]
fn malformed_input_is_rejected_with_a_reason() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();
        let add = format!("/admin/structure/menus/{menu}/add");

        let cases: [(&str, &str, &str); 5] = [
            ("https://example.com/", "Off site", "must be local"),
            ("//example.com/", "Protocol relative", "protocol-relative"),
            ("relative", "Relative", "absolute path"),
            // The assertion avoids the quotes on purpose: the message reads
            // "must not contain '..'", and Tera escapes the apostrophes.
            ("/a/../../etc/passwd", "Traversal", "must not contain"),
            ("", "No path", "required"),
        ];

        for (bad_path, title, expected) in cases {
            let (fresh_cookies, form) = get_page(app, &cookies, &add).await;
            let token = csrf_from(&form);
            let response = post_form(
                app,
                &fresh_cookies,
                &add,
                &[
                    ("_token", &token),
                    ("title", title),
                    ("path", bad_path),
                    ("parent_id", ""),
                    ("weight", "0"),
                ],
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{bad_path:?} should re-render the form"
            );
            let html = body_text(response).await;
            assert!(
                html.contains(expected),
                "{bad_path:?} should be refused with a message containing {expected:?}, got: {html}"
            );
        }

        let landed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link WHERE menu_name = $1")
            .bind(&menu)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(landed, 0, "no invalid link may have been written");
    });
}

/// A title is required.
#[test]
fn a_link_without_a_title_is_rejected() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();
        let add = format!("/admin/structure/menus/{menu}/add");

        let (cookies, form) = get_page(app, &cookies, &add).await;
        let token = csrf_from(&form);
        let response = post_form(
            app,
            &cookies,
            &add,
            &[
                ("_token", &token),
                ("title", "   "),
                ("path", &format!("/{menu}/untitled")),
                ("parent_id", ""),
                ("weight", "0"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("Title is required"));
    });
}

/// A parent in a different menu is refused: it could not render as an ancestor,
/// so accepting it would be a lie about the tree.
#[test]
fn a_parent_from_another_menu_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let here = fresh_menu();
        let elsewhere = fresh_menu();

        let foreign = create_link(
            app,
            &cookies,
            &elsewhere,
            "Foreign",
            &format!("/{elsewhere}/foreign"),
            None,
            0,
            false,
        )
        .await;

        let add = format!("/admin/structure/menus/{here}/add");
        let (cookies, form) = get_page(app, &cookies, &add).await;
        let token = csrf_from(&form);
        let response = post_form(
            app,
            &cookies,
            &add,
            &[
                ("_token", &token),
                ("title", "Adopted"),
                ("path", &format!("/{here}/adopted")),
                ("parent_id", &foreign.to_string()),
                ("weight", "0"),
            ],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            body_text(response)
                .await
                .contains("must be a link in this menu"),
            "a cross-menu parent must be refused with a reason"
        );
    });
}

// =============================================================================
// Delete
// =============================================================================

/// Deleting a link promotes its children to the deleted link's own parent, which
/// is what the listing says will happen.
#[test]
fn deleting_a_link_promotes_its_children_to_its_own_parent() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();

        let root = create_link(
            app,
            &cookies,
            &menu,
            "Root",
            &format!("/{menu}/root"),
            None,
            0,
            false,
        )
        .await;
        let middle = create_link(
            app,
            &cookies,
            &menu,
            "Middle",
            &format!("/{menu}/middle"),
            Some(root),
            0,
            false,
        )
        .await;
        let leaf = create_link(
            app,
            &cookies,
            &menu,
            "Leaf",
            &format!("/{menu}/leaf"),
            Some(middle),
            0,
            false,
        )
        .await;

        let (cookies, html) =
            get_page(app, &cookies, &format!("/admin/structure/menus/{menu}")).await;
        assert!(
            html.contains("promotes those children"),
            "the listing must say what happens to children"
        );
        let token = csrf_from(&html);

        let response = post_form(
            app,
            &cookies,
            &format!("/admin/structure/menus/{menu}/{middle}/delete"),
            &[("_token", &token)],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link WHERE id = $1")
            .bind(middle)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(gone, 0, "the deleted link must be gone");

        let leaf_parent: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_id FROM menu_link WHERE id = $1")
                .bind(leaf)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(
            leaf_parent,
            Some(root),
            "the child must be promoted to the deleted link's parent, not orphaned"
        );
    });
}

// =============================================================================
// Plugin-owned links
// =============================================================================

/// A `menu_link` row owned by a plugin is listed, labelled, and not editable.
#[test]
fn a_plugin_owned_link_is_listed_read_only_and_refuses_edits() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let menu = fresh_menu();

        // A core link as well as the plugin-owned one. Two reasons: a listing of
        // only plugin-owned rows renders no form and so carries no CSRF token to
        // read, and a mixed menu is the case worth asserting on anyway.
        create_link(
            app,
            &cookies,
            &menu,
            "Editable",
            &format!("/{menu}/editable"),
            None,
            0,
            false,
        )
        .await;

        // Insert a plugin-owned row directly: no form can create one, which is
        // the property under test.
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO menu_link (id, menu_name, path, title, weight, hidden, plugin, created, changed) \
             VALUES ($1, $2, $3, $4, 0, false, 'trovato_blog', $5, $5)",
        )
        .bind(id)
        .bind(&menu)
        .bind(format!("/{menu}/owned"))
        .bind("Owned by a plugin")
        .bind(now)
        .execute(&app.db)
        .await
        .unwrap();

        let (cookies, html) =
            get_page(app, &cookies, &format!("/admin/structure/menus/{menu}")).await;
        assert!(html.contains("Owned by a plugin"), "it must be listed");
        assert!(
            html.contains("trovato_blog") && html.contains("read-only"),
            "it must be attributed to its plugin and marked read-only, got: {html}"
        );
        assert!(
            !html.contains(&format!("/admin/structure/menus/{menu}/{id}/edit")),
            "no edit link may be offered for a plugin-owned row"
        );

        // And the routes refuse it even when the URL is typed by hand.
        let response = app
            .request_with_cookies(
                Request::get(format!("/admin/structure/menus/{menu}/{id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "editing a plugin-owned link must be refused"
        );

        let token = csrf_from(&html);
        let response = post_form(
            app,
            &cookies,
            &format!("/admin/structure/menus/{menu}/{id}/delete"),
            &[("_token", &token)],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "deleting a plugin-owned link must be refused"
        );

        let still_there: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(still_there, 1, "the refused delete must not have happened");
    });
}

/// The index lists plugin-registered navigation, from the registry rather than
/// from any table, and says it is read-only.
#[test]
fn the_index_lists_plugin_registered_navigation_as_read_only() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        let (_, html) = get_page(app, &cookies, "/admin/structure/menus").await;
        assert!(
            html.contains("Plugin-registered navigation"),
            "the index must have a section for tap_menu entries"
        );
        assert!(
            html.contains("not editable here"),
            "the section must say it is read-only, got: {html}"
        );
        // The two menus the theme renders are always offered.
        assert!(html.contains("/admin/structure/menus/main"));
        assert!(html.contains("/admin/structure/menus/footer"));
    });
}

// =============================================================================
// The rendered site
// =============================================================================

/// A link created through the form appears in the site's navigation on the next
/// request, with no restart.
///
/// This is the property that would have needed kernel plumbing if the menu were
/// read from a registry built at startup. It is not: `inject_site_context`
/// queries `menu_link` per render. The test pins that, so a future change to a
/// cached registry cannot quietly break it.
#[test]
fn a_new_link_appears_in_the_rendered_navigation_without_a_restart() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let marker = format!("Nav{}", Uuid::now_v7().simple());

        // Before: the marker is nowhere in the front page.
        let front = app
            .request(Request::get("/").body(Body::empty()).unwrap())
            .await;
        let before = body_text(front).await;
        assert!(!before.contains(&marker));

        // The theme renders the "main" menu, so the link goes there.
        let id = create_link(
            app,
            &cookies,
            "main",
            &marker,
            &format!("/nav-{}", Uuid::now_v7().simple()),
            None,
            500,
            false,
        )
        .await;

        let front = app
            .request(Request::get("/").body(Body::empty()).unwrap())
            .await;
        let after = body_text(front).await;
        assert!(
            after.contains(&marker),
            "a link created through the form must render in the navigation immediately"
        );

        // Hiding it takes it back out, again without a restart.
        let edit = format!("/admin/structure/menus/main/{id}/edit");
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let token = csrf_from(&form);
        let path: String = sqlx::query_scalar("SELECT path FROM menu_link WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("title", &marker),
                ("path", &path),
                ("parent_id", ""),
                ("weight", "500"),
                ("hidden", "1"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let front = app
            .request(Request::get("/").body(Body::empty()).unwrap())
            .await;
        let hidden_render = body_text(front).await;
        assert!(
            !hidden_render.contains(&marker),
            "hiding a link must take it out of the navigation immediately"
        );

        // Clean up: this test writes into the shared "main" menu.
        sqlx::query("DELETE FROM menu_link WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
    });
}
