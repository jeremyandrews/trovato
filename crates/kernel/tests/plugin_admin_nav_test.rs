#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin's own admin page gets the kernel's admin sidebar, and a plugin can
//! add its own link and card to it (BL-162).
//!
//! Before this existed, `routes::plugin_api::themed_response` rendered a
//! plugin's `/admin/...` page into `page--admin.html` without ever building
//! the `admin_nav` context `helpers::render_admin_template` builds for a
//! kernel admin page, so the sidebar was empty. And there was no way for a
//! plugin to add a link or a card to that sidebar at all: `routes::admin_nav`
//! `LINKS` and `CARDS` were fixed Rust arrays. `plugins/test_plugin_api`
//! declares one admin screen — `/admin/tpa/dashboard`, gated on `write test
//! notes` — through `tap_admin_nav`, the fixture this file drives.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_plugin_api/`, exactly like `plugin_api_test.rs`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;

const PLUGIN: &str = "test_plugin_api";
const PERM_WRITE: &str = "write test notes";
const DASHBOARD_PATH: &str = "/admin/tpa/dashboard";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("plugin admin nav fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
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

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        String::from_utf8_lossy(&bytes).replace("&#x2F;", "/"),
    )
}

/// The `href`s inside the admin sidebar, same extraction `admin_sidebar_test`
/// uses.
fn sidebar_links(html: &str) -> Vec<String> {
    let start = html
        .find(r#"<nav aria-label="Admin navigation">"#)
        .expect("the admin layout has its sidebar");
    let end = start + html[start..].find("</nav>").unwrap();
    html[start..end]
        .match_indices("href=\"")
        .map(|(at, _)| {
            let from = start + at + 6;
            html[from..from + html[from..].find('"').unwrap()].to_string()
        })
        .collect()
}

async fn disable_plugin(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = ANY($1)")
        .bind(vec![PLUGIN.to_string()])
        .execute(&app.db)
        .await
        .ok();
}

/// **The finding, closed.** A plugin's own admin page, reached through
/// `themed_response`, renders inside the kernel's `page--admin.html` with the
/// real sidebar — not the empty one `themed_response` used to build — and the
/// plugin's own link in it, for an administrator.
#[test]
fn a_plugin_admin_page_renders_the_kernel_sidebar_with_the_plugins_own_link() {
    common::run_test(async {
        let app = app();
        let (_id, cookies) = common::user_holding(
            app,
            "b162admin",
            &[
                "access administration pages",
                "edit any content",
                PERM_WRITE,
            ],
        )
        .await;

        let (status, html) = get(app, &cookies, DASHBOARD_PATH).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(r#"<nav aria-label="Admin navigation">"#),
            "the plugin's admin page must render inside the kernel admin shell: {html}"
        );
        let links = sidebar_links(&html);
        assert!(
            links.iter().any(|l| l == DASHBOARD_PATH),
            "the plugin's own link must be in its own sidebar: {links:?}"
        );
        assert!(
            html.contains("tpa-admin-content"),
            "the plugin's own content must still render: {html}"
        );

        disable_plugin(app).await;
    });
}

/// A viewer without the plugin link's permission does not see it — on the
/// plugin's own admin page, and on a kernel one.
#[test]
fn a_viewer_without_the_permission_does_not_see_the_plugin_link() {
    common::run_test(async {
        let app = app();
        let (_id, cookies) =
            common::user_holding(app, "b162nolink", &["access administration pages"]).await;

        // The route's own check refuses the page itself.
        let (status, _) = get(app, &cookies, DASHBOARD_PATH).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // And the link is absent from a kernel admin page's sidebar too — the
        // sidebar is filtered the same way everywhere, not only on the
        // plugin's own page.
        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        let links = sidebar_links(&html);
        assert!(
            !links.iter().any(|l| l == DASHBOARD_PATH),
            "a viewer without {PERM_WRITE:?} must not see the plugin's link: {links:?}"
        );
    });
}

/// A plugin-declared card appears on `/admin` only for a viewer holding its
/// permission, and the kernel's own cards are unaffected either way.
#[test]
fn the_dashboard_card_is_gated_on_the_plugins_permission_and_kernel_cards_are_unchanged() {
    common::run_test(async {
        let app = app();
        let (_id, holder_cookies) = common::user_holding(
            app,
            "b162card",
            &[
                "access administration pages",
                "edit any content",
                PERM_WRITE,
            ],
        )
        .await;
        let (_id2, other_cookies) = common::user_holding(
            app,
            "b162nocard",
            &["access administration pages", "edit any content"],
        )
        .await;

        let (status, with_card) = get(app, &holder_cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            with_card.contains("Notes written through the test plugin API."),
            "the holder must see the plugin's card"
        );

        let (status, without_card) = get(app, &other_cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !without_card.contains("Notes written through the test plugin API."),
            "a viewer without the permission must not see the plugin's card"
        );

        // Neither response's sidebar or dashboard dropped a kernel entry both
        // viewers hold the permission for.
        for html in [&with_card, &without_card] {
            assert!(
                sidebar_links(html).iter().any(|l| l == "/admin/content"),
                "a kernel link must still be offered: {html}"
            );
            assert!(
                html.contains("/admin/content\""),
                "the kernel's Content card must still be offered"
            );
        }

        disable_plugin(app).await;
    });
}
