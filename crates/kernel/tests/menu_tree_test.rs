#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Menus as templates get them: every config menu by name, child links nested
//! under their parent, plugin entries under their parent, an active trail that
//! follows a section, a `menu` tile with real links, and all of it without a
//! menu or alias query per render.
//!
//! Each test drives the real router and reads the stock `templates/page.html`.
//! The plugin entries come from `plugins/test_menu_entries`, a real wasm plugin
//! registering three entries under `/user`; this file installs it before its app
//! is built, because `AppState` resolves the enabled plugins once.
//!
//! Every menu link, tile and alias a test makes is removed again when it ends:
//! the database is shared, and a stray `main` link would sit in every other
//! test's navigation.
//!
//! The query count test reads a process-wide cache, so it assumes no other test
//! in this binary writes a menu link between its two requests; CI runs with
//! `--test-threads=1`.
//!
//! Requires Postgres + Redis and `plugins/test_menu_entries/test_menu_entries.wasm`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateMenuLink, CreateUrlAlias, MenuLink, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PLUGIN: &str = "test_menu_entries";
const PERM_EXTRA: &str = "see test menu extra";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("menu tree fixture app init thread panicked")
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

fn stamp() -> String {
    Uuid::now_v7().simple().to_string()
}

/// Add a Live menu link, removed again when the test ends.
async fn link(app: &TestApp, menu: &str, path: &str, title: &str, parent: Option<Uuid>) -> Uuid {
    let created = MenuLink::create(
        &app.db,
        CreateMenuLink {
            menu_name: Some(menu.to_string()),
            path: path.to_string(),
            title: title.to_string(),
            parent_id: parent,
            weight: Some(0),
            hidden: Some(false),
            plugin: None,
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create menu link");
    let pool = app.db.clone();
    let id = created.id;
    defer_cleanup(move || async move {
        let _ = MenuLink::delete(&pool, id).await;
    });
    id
}

/// A published conference reachable at `alias`, removed again when the test ends.
async fn page_at(app: &TestApp, alias: &str, title: &str) -> Uuid {
    app.ensure_conference_type().await;
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("menu tree test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create item")
        .id;
    let alias_row = UrlAlias::create(
        &app.db,
        CreateUrlAlias {
            source: format!("/item/{id}"),
            alias: alias.to_string(),
            language: Some("en".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create alias");
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = UrlAlias::delete(&pool, alias_row.id).await;
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    // Autoescape writes `/` as `&#x2F;`, which a browser reads as `/`; read
    // the page the way a browser does.
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

/// The opening tag of the first navigation link to exactly `href`.
fn anchor<'a>(html: &'a str, href: &str) -> Option<&'a str> {
    let needle = format!("<a href=\"{href}\" class=\"site-nav__link");
    let start = html.find(&needle)?;
    let end = html[start..].find('>')? + start + 1;
    Some(&html[start..end])
}

fn is_active(tag: &str) -> bool {
    tag.contains("site-nav__link--active") && tag.contains("aria-current=\"page\"")
}

/// A member holding `permissions` (through a role when any), and their cookies.
async fn member(app: &TestApp, permissions: &[&str]) -> (Uuid, String) {
    common::user_holding(app, "b74menu", permissions).await
}

#[test]
fn a_config_menu_named_user_reaches_the_page() {
    run_test(async {
        let app = app();
        let s = stamp();
        let title = format!("B74 user menu {s}");
        link(app, "user", &format!("/b74-user-{s}"), &title, None).await;

        let (_, cookies) = member(app, &[]).await;
        let html = get(app, "/user/profile", &cookies).await;
        let account = html
            .find("class=\"site-nav__account\"")
            .map(|at| &html[at..])
            .unwrap_or_else(|| panic!("no account menu on the page:\n{html}"));
        assert!(
            account.contains(&format!("href=\"/b74-user-{s}\"")),
            "the `user` menu is drawn in the account area:\n{html}"
        );
    });
}

#[test]
fn a_child_link_renders_nested_under_its_parent() {
    run_test(async {
        let app = app();
        let s = stamp();
        let parent = link(
            app,
            "main",
            &format!("/b74-parent-{s}"),
            &format!("B74 parent {s}"),
            None,
        )
        .await;
        link(
            app,
            "main",
            &format!("/b74-parent-{s}/child"),
            &format!("B74 child {s}"),
            Some(parent),
        )
        .await;

        let (_, cookies) = member(app, &[]).await;
        let html = get(app, "/user/profile", &cookies).await;
        let parent_at = html
            .find(&format!("<a href=\"/b74-parent-{s}\""))
            .unwrap_or_else(|| panic!("parent link missing:\n{html}"));
        let child_at = html
            .find(&format!("<a href=\"/b74-parent-{s}/child\""))
            .unwrap_or_else(|| panic!("child link missing:\n{html}"));
        assert!(parent_at < child_at, "{html}");
        let between = &html[parent_at..child_at];
        assert!(
            between.contains("<ul class=\"site-nav__children\""),
            "the child sits in a list nested under its parent, not beside it:\n{between}"
        );
        assert!(
            !between.contains("</li>"),
            "nothing closes the parent's item before the child:\n{between}"
        );
    });
}

#[test]
fn a_child_under_a_link_the_viewer_may_not_follow_is_dropped() {
    run_test(async {
        let app = app();
        let s = stamp();
        // The plugin's gated entry: only a holder of PERM_EXTRA may follow it.
        let parent = link(
            app,
            "main",
            "/test-menu-entries/extra",
            &format!("B74 gated {s}"),
            None,
        )
        .await;
        link(
            app,
            "main",
            &format!("/b74-under-gated-{s}"),
            &format!("B74 under gated {s}"),
            Some(parent),
        )
        .await;

        let (_, plain) = member(app, &[]).await;
        let html = get(app, "/user/profile", &plain).await;
        assert!(!html.contains(&format!("B74 gated {s}")), "{html}");
        assert!(!html.contains(&format!("B74 under gated {s}")), "{html}");

        let (_, holder) = member(app, &[PERM_EXTRA]).await;
        let html = get(app, "/user/profile", &holder).await;
        assert!(html.contains(&format!("B74 gated {s}")), "{html}");
        assert!(html.contains(&format!("B74 under gated {s}")), "{html}");
    });
}

#[test]
fn a_plugin_entry_under_user_is_drawn_for_a_viewer_with_its_permission() {
    run_test(async {
        let app = app();

        let (_, holder) = member(app, &[PERM_EXTRA]).await;
        let html = get(app, "/user/profile", &holder).await;
        assert!(
            anchor(&html, "/test-menu-entries/extra").is_some(),
            "the gated entry is in the account menu of a viewer holding its permission:\n{html}"
        );

        let (_, plain) = member(app, &[]).await;
        let html = get(app, "/user/profile", &plain).await;
        assert!(
            anchor(&html, "/test-menu-entries/extra").is_none(),
            "{html}"
        );
    });
}

#[test]
fn a_uid_entry_is_drawn_with_the_viewers_id_and_never_for_an_anonymous_visitor() {
    run_test(async {
        let app = app();

        let (uid, cookies) = member(app, &[]).await;
        let html = get(app, "/user/profile", &cookies).await;
        assert!(
            anchor(&html, &format!("/user/{uid}/test-menu-entries")).is_some(),
            "the viewer's own entry carries their id:\n{html}"
        );
        assert!(!html.contains("{uid}"), "{html}");
        assert!(!html.contains("Test menu unfollowable"), "{html}");

        let html = get(app, "/user/login", "").await;
        assert!(!html.contains("Test menu mine"), "{html}");
        assert!(!html.contains("{uid}"), "{html}");
        assert!(!html.contains("Test menu unfollowable"), "{html}");
    });
}

#[test]
fn a_link_is_active_on_every_page_under_its_path_and_home_only_on_home() {
    run_test(async {
        let app = app();
        let s = stamp();
        let section = format!("/b74-devices-{s}");
        link(app, "main", &section, &format!("B74 devices {s}"), None).await;
        link(app, "main", "/", &format!("B74 home {s}"), None).await;
        page_at(
            app,
            &format!("{section}/new"),
            &format!("B74 new device {s}"),
        )
        .await;

        let html = get(app, &format!("{section}/new"), "").await;
        let devices = anchor(&html, &section).unwrap_or_else(|| panic!("no section link:\n{html}"));
        assert!(is_active(devices), "{devices}");
        let home = anchor(&html, "/").unwrap_or_else(|| panic!("no home link:\n{html}"));
        assert!(!is_active(home), "`/` is active only on `/`: {home}");

        let html = get(app, "/", "").await;
        let home = anchor(&html, "/").unwrap_or_else(|| panic!("no home link:\n{html}"));
        assert!(is_active(home), "{home}");
        let devices = anchor(&html, &section).unwrap_or_else(|| panic!("no section link:\n{html}"));
        assert!(!is_active(devices), "{devices}");
    });
}

/// Register a gather with no rows and return its id. Its page,
/// `/gather/{id}`, is drawn through `page.html`'s own content block, so it
/// carries the sidebar; the account pages replace that block and have none.
async fn empty_gather(app: &TestApp, s: &str) -> String {
    let query_id = format!("b74_empty_{s}");
    let query: trovato_kernel::gather::GatherQuery = serde_json::from_value(serde_json::json!({
        "query_id": query_id,
        "label": "B74 empty gather",
        "definition": {
            "base_table": "item",
            "item_type": "conference",
            "filters": [{
                "exposed": false,
                "field": "title",
                "operator": "equals",
                "value": format!("b74-no-such-title-{s}"),
            }],
        },
        "display": { "format": "table", "items_per_page": 10 },
        "plugin": "core"
    }))
    .expect("gather deserializes");
    app.state
        .gather()
        .register_query(query)
        .await
        .expect("register gather");
    let gathers = app.state.clone();
    let cleanup_id = query_id.clone();
    defer_cleanup(move || async move {
        let _ = gathers.gather().delete_query(&cleanup_id).await;
    });
    query_id
}

#[test]
fn a_menu_tile_renders_its_menu_filtered_for_the_viewer() {
    run_test(async {
        use trovato_kernel::models::tile::{CreateTile, Tile};

        let app = app();
        let s = stamp();
        let menu = format!("b74tile{s}");
        link(
            app,
            &menu,
            &format!("/b74-tile-public-{s}"),
            &format!("B74 tile public {s}"),
            None,
        )
        .await;
        link(
            app,
            &menu,
            "/test-menu-entries/extra",
            &format!("B74 tile gated {s}"),
            None,
        )
        .await;
        let tile = Tile::create(
            &app.db,
            CreateTile {
                machine_name: format!("b74_menu_tile_{s}"),
                label: format!("B74 menu tile {s}"),
                region: Some("sidebar".to_string()),
                tile_type: Some("menu".to_string()),
                config: Some(serde_json::json!({ "menu_name": menu })),
                visibility: Some(serde_json::json!({})),
                weight: Some(0),
                status: Some(1),
                plugin: None,
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create tile");
        let pool = app.db.clone();
        defer_cleanup(move || async move {
            let _ = Tile::delete(&pool, tile.id).await;
        });

        let page = format!("/gather/{}", empty_gather(app, &s).await);
        let (_, plain) = member(app, &[]).await;
        let html = get(app, &page, &plain).await;
        let at = html
            .find(&format!("id=\"tile-b74_menu_tile_{s}\""))
            .unwrap_or_else(|| panic!("no menu tile:\n{html}"));
        let tile_html = &html[at..];
        assert!(
            tile_html.contains(&format!("<a href=\"/b74-tile-public-{s}\"")),
            "the tile lists its menu's links:\n{tile_html}"
        );
        assert!(!html.contains(&format!("B74 tile gated {s}")), "{html}");

        let (_, holder) = member(app, &[PERM_EXTRA]).await;
        let html = get(app, &page, &holder).await;
        assert!(html.contains(&format!("B74 tile gated {s}")), "{html}");
    });
}

// -----------------------------------------------------------------------------
// Query counts: menus cost no menu or alias query on a warm cache.
// -----------------------------------------------------------------------------

/// Records every statement sqlx runs, from its `sqlx::query` events.
struct Statements(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

struct StatementText(String);

impl tracing::field::Visit for StatementText {
    fn record_debug(&mut self, _field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{value:?} "));
    }

    fn record_str(&mut self, _field: &tracing::field::Field, value: &str) {
        self.0.push_str(value);
        self.0.push(' ');
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Statements {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == "sqlx::query" {
            let mut text = StatementText(String::new());
            event.record(&mut text);
            self.0.lock().unwrap().push(text.0);
        }
    }
}

/// Every statement one GET runs.
async fn statements_for(app: &TestApp, path: &str, cookies: &str) -> Vec<String> {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(Statements(seen.clone()));
    get(app, path, cookies).with_subscriber(subscriber).await;
    seen.lock().unwrap().clone()
}

#[test]
fn a_warm_page_with_four_menus_runs_no_menu_or_alias_query() {
    run_test(async {
        let app = app();
        let s = stamp();

        // Four config menus, one link of them to a translated page by its alias.
        let target = page_at(app, &format!("/b74-qc-{s}"), &format!("B74 qc page {s}")).await;
        sqlx::query(
            "INSERT INTO item_translation (item_id, language, title, fields) \
             VALUES ($1, 'it', $2, '{}'::jsonb) ON CONFLICT (item_id, language) DO NOTHING",
        )
        .bind(target)
        .bind(format!("B74 pagina {s}"))
        .execute(&app.db)
        .await
        .expect("translate");
        link(
            app,
            "main",
            &format!("/b74-qc-{s}"),
            &format!("B74 qc page {s}"),
            None,
        )
        .await;
        link(
            app,
            "footer",
            &format!("/b74-qc-footer-{s}"),
            &format!("B74 qc footer {s}"),
            None,
        )
        .await;
        link(
            app,
            "user",
            &format!("/b74-qc-user-{s}"),
            &format!("B74 qc user {s}"),
            None,
        )
        .await;
        link(
            app,
            &format!("b74qc{s}"),
            &format!("/b74-qc-x-{s}"),
            &format!("B74 qc x {s}"),
            None,
        )
        .await;

        // A gather with no rows, read in Italian: a page that renders the
        // localized menus and has no alias lookups of its own.
        let query_id = empty_gather(app, &s).await;

        let (_, cookies) = member(app, &[]).await;
        let path = format!("/it/gather/{query_id}");

        // Warm, then count, once the writes above have had their cache
        // generations seen and cleared.
        common::settle_cache_generations().await;
        let warm = get(app, &path, &cookies).await;
        assert!(
            warm.contains(&format!("B74 pagina {s}")),
            "the menu is localized for Italian: {warm}"
        );
        let statements = statements_for(app, &path, &cookies).await;
        assert!(!statements.is_empty(), "the counter sees statements at all");
        // The one alias statement a request always runs is the path alias
        // middleware resolving the requested path itself (`WHERE alias = $1`);
        // the menus resolve their links by source, never by alias.
        let menu_queries: Vec<&String> = statements
            .iter()
            .filter(|sql| sql.contains("menu_link") || sql.contains("url_alias"))
            .filter(|sql| !sql.contains("WHERE alias = $1"))
            .collect();
        assert!(
            menu_queries.is_empty(),
            "a warm render runs no menu_link or url_alias query, ran {}:\n{menu_queries:#?}",
            menu_queries.len()
        );
    });
}
