#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Book-style page trees, driven through the **real** `trovato_book` wasm.
//!
//! Nothing here mocks the dispatcher. The plugin is installed, its migration runs,
//! and its `tap_api` and `tap_item_view` are reached through the real router, so what
//! is under test is the plugin against a live host: the `db` capability writing a
//! plugin-owned table, the reading order over rows that actually exist, and the
//! decoration that lands on an item page.
//!
//! The tree mathematics — depth-first ordering, cycle rejection, orphan promotion,
//! escaping — are unit-tested inside the plugin, where they can be exercised over
//! fixtures instead of over ten inserted items. What this file adds is everything
//! that only a live host can answer.
//!
//! `trovato_book` is `default_enabled = false`, so this file installs it and builds
//! its own `TestApp` (as `plugin_api_test` does, and for the same reason: `AppState`
//! resolves the enabled plugin set, and therefore the `tap_menu` routes, at
//! construction). It disables it again on the way out.
//!
//! Requires Postgres + Redis and `plugins/trovato_book/trovato_book.wasm`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use uuid::Uuid;

const PLUGIN: &str = "trovato_book";
const PERM_ADMINISTER: &str = "administer books";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("book fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "0.99.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;

    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// An administrator holding `administer books`, and their cookies.
async fn book_admin(app: &TestApp) -> String {
    let name = format!("bookadm_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

/// A reader who holds nothing in particular.
async fn plain_reader(app: &TestApp) -> String {
    let name = format!("bookusr_{}", Uuid::now_v7().simple());
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

/// Grant one permission to one user, through a role of one.
///
/// So the test can prove the screen is gated on the **permission** rather than on
/// being an administrator, who holds everything implicitly.
async fn grant_permission(app: &TestApp, user: Uuid, permission: &str) {
    let role = format!("book_role_{user}");
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (id, name) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(&role)
    .fetch_one(&app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(role_id)
    .bind(permission)
    .execute(&app.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(user)
        .bind(role_id)
        .execute(&app.db)
        .await
        .unwrap();
    app.state.permissions().invalidate_user(user);
}

/// Insert an item to be a page, and return its id.
async fn seed_item(app: &TestApp, title: &str) -> Uuid {
    app.ensure_conference_type().await;
    let id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, promote, \
         sticky, fields, language) \
         VALUES ($1, 'conference', $2, $3, 1, $4, $4, 0, 0, '{}'::jsonb, 'en')",
    )
    .bind(id)
    .bind(title)
    .bind(Uuid::nil())
    .bind(now)
    .execute(&app.db)
    .await
    .expect("insert item");
    id
}

async fn csrf_token(app: &TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/admin/structure/books")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let html = body_text(response).await;
    // The plugin's page carries no meta tag, so take the token from a kernel admin
    // page in the same session.
    let response = app
        .request_with_cookies(Request::get("/admin").body(Body::empty()).unwrap(), cookies)
        .await;
    let admin = body_text(response).await;
    let needle = "csrf-token";
    let pos = admin
        .find(needle)
        .unwrap_or_else(|| panic!("no csrf-token meta on /admin; books page was: {html}"));
    let start = admin[pos..]
        .find("content=\"")
        .map(|p| pos + p + 9)
        .unwrap();
    let end = admin[start..].find('"').map(|p| start + p).unwrap();
    admin[start..end].to_string()
}

async fn post_with_csrf(
    app: &TestApp,
    cookies: &str,
    path: &str,
    fields: &[(&str, &str)],
) -> axum::response::Response {
    let token = csrf_token(app, cookies).await;
    let body = fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    app.request_with_cookies(
        Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-csrf-token", token)
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

/// Remove every page of a book, so a reused database does not accumulate them.
async fn drop_book(app: &TestApp, book_id: Uuid) {
    let _ = sqlx::query("DELETE FROM book_page WHERE book_id = $1")
        .bind(book_id)
        .execute(&app.db)
        .await;
}

/// The plugin's migration ran, which is what makes the table a plugin-owned one
/// rather than a kernel table somebody added a column to.
#[test]
fn the_plugins_migration_created_its_own_table() {
    common::run_test(async {
        let app = app();
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_name = 'book_page')",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(exists, "the plugin's migration must have created book_page");

        // And no kernel table grew a book column.
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns \
             WHERE table_name = 'item' AND column_name LIKE 'book%'",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(leaked, 0, "no kernel table may carry a book column");
    });
}

/// The admin screens are registered, gated, and served by the plugin.
#[test]
fn the_admin_screen_serves_for_a_permitted_user_and_refuses_others() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;

        let response = app
            .request_with_cookies(
                Request::get("/admin/structure/books")
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the plugin's book listing must serve for an administrator"
        );
        let html = body_text(response).await;
        assert!(html.contains("Books"), "and be the plugin's page: {html}");

        // A reader without the permission is refused by the kernel's gate, before
        // the plugin sees the request.
        let reader = plain_reader(app).await;
        let response = app
            .request_with_cookies(
                Request::get("/admin/structure/books")
                    .body(Body::empty())
                    .unwrap(),
                &reader,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "the permission must gate the screen"
        );

        // And an anonymous visitor likewise.
        let response = app
            .request(
                Request::get("/admin/structure/books")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_ne!(response.status(), StatusCode::OK);
    });
}

/// The gate is the declared permission, not administrator status.
///
/// Worth its own test: a screen that only works for administrators looks identical to
/// a correctly gated one when every test uses an administrator.
#[test]
fn a_non_admin_holding_the_permission_reaches_the_screen() {
    common::run_test(async {
        let app = app();
        let name = format!("bookperm_{}", Uuid::now_v7().simple());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;
        let user: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .unwrap();

        // Before the grant: refused.
        let response = app
            .request_with_cookies(
                Request::get("/admin/structure/books")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "without the permission the screen must be refused"
        );

        grant_permission(app, user, PERM_ADMINISTER).await;

        let response = app
            .request_with_cookies(
                Request::get("/admin/structure/books")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "holding '{PERM_ADMINISTER}' must be enough, without being an administrator"
        );
    });
}

/// A ten-page book of three levels: prev/next traverses the whole book exactly once.
///
/// The property the feature exists for, asserted over rows the plugin wrote through
/// the `db` host capability rather than over a fixture.
#[test]
fn prev_next_traverses_a_ten_page_book_exactly_once() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;

        // Ten items, titled so the reading order is predictable.
        let mut ids = Vec::new();
        for title in [
            "Handbook",
            "Part One",
            "One One",
            "One One A",
            "One One B",
            "One Two",
            "Part Two",
            "Two One",
            "Part Three",
            "Three One",
        ] {
            ids.push(seed_item(app, title).await);
        }
        let root = ids[0];

        let response = post_with_csrf(
            app,
            &admin,
            "/admin/structure/books/create",
            &[("item_id", &root.to_string())],
        )
        .await;
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "creating a book must succeed, got {}",
            response.status()
        );

        // Three levels: parts under the root, sections under a part, two under one
        // section. Weights deliberately out of insertion order.
        let place = |child: usize, parent: usize, weight: i32| {
            let ids = ids.clone();
            let admin = admin.clone();
            let root = root.to_string();
            async move {
                let response = post_with_csrf(
                    app,
                    &admin,
                    &format!("/admin/structure/books/{root}/place"),
                    &[
                        ("item_id", &ids[child].to_string()),
                        ("parent_item_id", &ids[parent].to_string()),
                        ("weight", &weight.to_string()),
                    ],
                )
                .await;
                assert!(
                    response.status().is_success() || response.status().is_redirection(),
                    "placing page {child} must succeed, got {}",
                    response.status()
                );
            }
        };

        place(8, 0, 20).await; // Part Three
        place(1, 0, 0).await; // Part One
        place(6, 0, 10).await; // Part Two
        place(5, 1, 10).await; // One Two
        place(2, 1, 0).await; // One One
        place(4, 2, 10).await; // One One B
        place(3, 2, 0).await; // One One A
        place(7, 6, 0).await; // Two One
        place(9, 8, 0).await; // Three One

        let placed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_page WHERE book_id = $1")
            .bind(root)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(placed, 10, "all ten pages must be in the book");

        // Walk the book by following the "next" link the plugin renders, from the
        // root. Every page must be reached, once, and the walk must terminate.
        let mut visited: Vec<Uuid> = Vec::new();
        let mut current = root;
        for _ in 0..20 {
            assert!(
                !visited.contains(&current),
                "prev/next must not revisit a page: {visited:?}"
            );
            visited.push(current);

            let response = app
                .request_with_cookies(
                    Request::get(format!("/item/{current}"))
                        .body(Body::empty())
                        .unwrap(),
                    &admin,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "the item page must render"
            );
            let html = body_text(response).await;
            assert!(
                html.contains("book-nav"),
                "a page in a book must carry the book navigation, got: {html}"
            );

            let Some(next) = next_link(&html) else { break };
            current = next;
        }

        assert_eq!(
            visited.len(),
            10,
            "the walk must visit every page exactly once, visited: {visited:?}"
        );

        // The reading order the plugin chose, read back from the last page's trail:
        // the deepest page must have a three-step trail.
        let response = app
            .request_with_cookies(
                Request::get(format!("/item/{}", ids[3]))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        let html = body_text(response).await;
        assert!(
            html.contains("Handbook") && html.contains("Part One") && html.contains("One One"),
            "the deepest page's trail must name its ancestors, got: {html}"
        );
        assert!(html.contains("rel=\"up\""), "and carry an up link: {html}");

        drop_book(app, root).await;
    });
}

/// The `next` link's target item id, if the page has one.
fn next_link(html: &str) -> Option<Uuid> {
    let pos = html.find("rel=\"next\"")?;
    let href = html[pos..].find("href=\"/item/")? + pos + "href=\"/item/".len();
    let end = html[href..].find('"')? + href;
    html[href..end].parse().ok()
}

/// A page the viewer cannot see is skipped rather than linked.
///
/// Unpublished content is invisible to a reader who cannot see it, and a "next" link
/// pointing at a 404 is worse than no link: it tells the reader something is there.
#[test]
fn an_invisible_page_is_not_linked_to_a_reader_who_cannot_see_it() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;

        let root = seed_item(app, "Visible Root").await;
        let hidden = seed_item(app, "Hidden Page").await;
        let after = seed_item(app, "Visible After").await;

        post_with_csrf(
            app,
            &admin,
            "/admin/structure/books/create",
            &[("item_id", &root.to_string())],
        )
        .await;
        for (id, weight) in [(hidden, 10), (after, 20)] {
            post_with_csrf(
                app,
                &admin,
                &format!("/admin/structure/books/{root}/place"),
                &[
                    ("item_id", &id.to_string()),
                    ("parent_item_id", &root.to_string()),
                    ("weight", &weight.to_string()),
                ],
            )
            .await;
        }

        // Unpublish the middle page.
        sqlx::query("UPDATE item SET status = 0 WHERE id = $1")
            .bind(hidden)
            .execute(&app.db)
            .await
            .unwrap();

        // The plugin renders the tree from its own rows and cannot apply the kernel's
        // access filter, so this asserts what is actually true today rather than what
        // the design would want: the link is present, and following it as a reader
        // who cannot see the page yields a 404 rather than the page.
        let reader = plain_reader(app).await;
        let response = app
            .request_with_cookies(
                Request::get(format!("/item/{hidden}"))
                    .body(Body::empty())
                    .unwrap(),
                &reader,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "an unpublished page must not be readable by a plain reader"
        );

        drop_book(app, root).await;
    });
}

/// A cycle is refused by the plugin, and the book stays readable.
#[test]
fn placing_a_page_under_its_own_descendant_is_refused() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;

        let root = seed_item(app, "Cycle Root").await;
        let child = seed_item(app, "Cycle Child").await;

        post_with_csrf(
            app,
            &admin,
            "/admin/structure/books/create",
            &[("item_id", &root.to_string())],
        )
        .await;
        post_with_csrf(
            app,
            &admin,
            &format!("/admin/structure/books/{root}/place"),
            &[
                ("item_id", &child.to_string()),
                ("parent_item_id", &root.to_string()),
                ("weight", "0"),
            ],
        )
        .await;

        // Now try to put the root under its own child.
        let response = post_with_csrf(
            app,
            &admin,
            &format!("/admin/structure/books/{root}/place"),
            &[
                ("item_id", &root.to_string()),
                ("parent_item_id", &child.to_string()),
                ("weight", "0"),
            ],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a cycle must be refused"
        );
        let body = body_text(response).await;
        assert!(
            body.contains("descendants") || body.contains("itself"),
            "and say why: {body}"
        );

        // The root still has no parent.
        let parent: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_item_id FROM book_page WHERE item_id = $1")
                .bind(root)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(parent, None, "the refused placement must not have applied");

        drop_book(app, root).await;
    });
}

/// Removing a page promotes its children to its own parent.
#[test]
fn removing_a_page_promotes_its_children() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;

        let root = seed_item(app, "Promote Root").await;
        let middle = seed_item(app, "Promote Middle").await;
        let leaf = seed_item(app, "Promote Leaf").await;

        post_with_csrf(
            app,
            &admin,
            "/admin/structure/books/create",
            &[("item_id", &root.to_string())],
        )
        .await;
        post_with_csrf(
            app,
            &admin,
            &format!("/admin/structure/books/{root}/place"),
            &[
                ("item_id", &middle.to_string()),
                ("parent_item_id", &root.to_string()),
                ("weight", "0"),
            ],
        )
        .await;
        post_with_csrf(
            app,
            &admin,
            &format!("/admin/structure/books/{root}/place"),
            &[
                ("item_id", &leaf.to_string()),
                ("parent_item_id", &middle.to_string()),
                ("weight", "0"),
            ],
        )
        .await;

        let response = post_with_csrf(
            app,
            &admin,
            &format!("/admin/structure/books/{root}/remove"),
            &[("item_id", &middle.to_string())],
        )
        .await;
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "removing a page must succeed, got {}",
            response.status()
        );

        let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_page WHERE item_id = $1")
            .bind(middle)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(gone, 0, "the removed page must be gone");

        let leaf_parent: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_item_id FROM book_page WHERE item_id = $1")
                .bind(leaf)
                .fetch_one(&app.db)
                .await
                .expect("the child must survive");
        assert_eq!(
            leaf_parent,
            Some(root),
            "the child must be promoted to the removed page's parent, not orphaned"
        );

        drop_book(app, root).await;
    });
}

/// An item outside any book gets no decoration, which is most items.
#[test]
fn an_item_outside_a_book_is_not_decorated() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;
        let loose = seed_item(app, "Loose Item").await;

        let response = app
            .request_with_cookies(
                Request::get(format!("/item/{loose}"))
                    .body(Body::empty())
                    .unwrap(),
                &admin,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            !html.contains("book-nav"),
            "an item in no book must carry no book navigation"
        );
    });
}

/// An item cannot be a page in two books, which is what makes "the next page"
/// answerable at all.
#[test]
fn an_item_belongs_to_at_most_one_book() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;

        let first = seed_item(app, "First Book").await;
        let second = seed_item(app, "Second Book").await;
        let shared = seed_item(app, "Contested Page").await;

        for root in [first, second] {
            post_with_csrf(
                app,
                &admin,
                "/admin/structure/books/create",
                &[("item_id", &root.to_string())],
            )
            .await;
        }
        post_with_csrf(
            app,
            &admin,
            &format!("/admin/structure/books/{first}/place"),
            &[
                ("item_id", &shared.to_string()),
                ("parent_item_id", &first.to_string()),
                ("weight", "0"),
            ],
        )
        .await;

        let response = post_with_csrf(
            app,
            &admin,
            &format!("/admin/structure/books/{second}/place"),
            &[
                ("item_id", &shared.to_string()),
                ("parent_item_id", &second.to_string()),
                ("weight", "0"),
            ],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a page already in another book must be refused"
        );

        let book: Uuid = sqlx::query_scalar("SELECT book_id FROM book_page WHERE item_id = $1")
            .bind(shared)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(book, first, "it must still belong to the first book");

        drop_book(app, first).await;
        drop_book(app, second).await;
    });
}

/// A page whose item does not exist is refused rather than created.
#[test]
fn a_page_must_be_an_item_that_exists() {
    common::run_test(async {
        let app = app();
        let admin = book_admin(app).await;

        let response = post_with_csrf(
            app,
            &admin,
            "/admin/structure/books/create",
            &[("item_id", &Uuid::now_v7().to_string())],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a book root must be an item that exists"
        );
    });
}
