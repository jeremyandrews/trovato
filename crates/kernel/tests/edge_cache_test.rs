#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Edge caching headers, surrogate keys and purge (B303).
//!
//! An anonymous, cookie-free, `GET`/`HEAD` response that answers 200 with
//! `text/html`, in a language the URL alone decided, may be marked
//! `Cache-Control: public` with a tag header listing what it was built from.
//! Everything else — a signed-in or token-bearing request, a non-`GET`/`HEAD`,
//! a response that sets a cookie, or a page whose language an
//! `Accept-Language` header (rather than the URL) decided — gets
//! `private, no-store` or nothing, never `public`.
//!
//! The shared test fixture ([`common::TestApp`]) always seeds three
//! languages (`en`, `it`, `he`; see `common::EXTRA_TEST_LANGUAGES`), so every
//! app built here is a multi-language site. That is exactly the case problem
//! 6 is about: an *unprefixed* page's language is decided by whatever
//! `Accept-Language` negotiation would choose for a given visitor, which is
//! never safe to cache (scenario (f)). Every "this should be public" scenario
//! below therefore reads the item under a URL prefix (`/it/...`): the one
//! source [`decide`](trovato_kernel::middleware::edge_cache) always considers
//! safe, whatever the site's language count is.
//!
//! Requires Postgres + Redis (fresh `TestApp`s; each test sets its own
//! `EDGE_CACHE_*` settings, so none can share the suite's `shared_app`).

mod common;

use std::sync::Mutex;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use common::{TestApp, run_test, username};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// This file's own content type, with no required fields, so a test can
/// create an item with nothing but a title. A type of its own (not
/// `conference`, which has required date fields) seeded under its own
/// advisory lock, mirroring `front_page_test.rs`'s `ensure_item_type`.
const ITEM_TYPE: &str = "edge_cache_test_item";

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_B303_0000_0001;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// Seed [`ITEM_TYPE`]. Idempotent, and safe against concurrent seeders.
async fn ensure_item_type(app: &TestApp) {
    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Edge Cache Test', 'Fixture type for edge cache tests', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed edge cache test item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .replace(
            ITEM_TYPE,
            "Edge Cache Test",
            Some("Fixture type for edge cache tests"),
            settings,
        )
        .await
        .ok();
}

/// Create a published item of [`ITEM_TYPE`] and return its id.
async fn create_published_item(app: &TestApp, title: &str, promote: i16) -> Uuid {
    ensure_item_type(app).await;
    app.state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(promote),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("edge cache test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// The item's address under the `it` language prefix: a `UrlPrefix` language
/// source, so whether it is cacheable never depends on the site's language
/// count (unlike the bare, unprefixed address — see scenario (f) below).
fn prefixed_item_path(id: Uuid) -> String {
    format!("/it/item/{id}")
}

fn unprefixed_item_path(id: Uuid) -> String {
    format!("/item/{id}")
}

async fn get(app: &TestApp, path: &str) -> Response {
    app.request(Request::get(path).body(Body::empty()).unwrap())
        .await
}

async fn get_with_cookies(app: &TestApp, path: &str, cookies: &str) -> Response {
    app.request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await
}

async fn head(app: &TestApp, path: &str) -> Response {
    app.request(Request::head(path).body(Body::empty()).unwrap())
        .await
}

fn header_str(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn body_string(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Set `language_prefix_default` before a `TestApp` is built: `AppState`
/// snapshots it at construction (see `LanguageSet::load`), so it has to be
/// written to the database before `TestApp::with_config` runs, on a pool of
/// its own, the same way `common`'s `seed_test_languages` does.
async fn seed_prefix_default(value: bool) {
    trovato_test_utils::env::load_dotenv();
    let pool = sqlx::PgPool::connect(&trovato_test_utils::env::database_url())
        .await
        .expect("connect to seed language_prefix_default");
    trovato_kernel::models::SiteConfig::set(
        &pool,
        trovato_kernel::middleware::language::PREFIX_DEFAULT_KEY,
        serde_json::json!(value),
    )
    .await
    .expect("seed language_prefix_default");
    pool.close().await;
}

/// This file builds a fresh `TestApp` per test (each needs its own
/// `EDGE_CACHE_*` settings), unlike most test files which share one. A small
/// pool keeps that from exhausting Postgres's `max_connections` when several
/// of this file's tests happen to run at once.
fn small_pool(c: &mut trovato_kernel::config::Config) {
    c.database_max_connections = 4;
}

/// Build a `TestApp` with edge caching on: 60s browser, 600s shared.
async fn app_with_edge_cache_on() -> TestApp {
    TestApp::with_config(|c| {
        small_pool(c);
        c.runtime.edge_cache.max_age = 60;
        c.runtime.edge_cache.s_maxage = 600;
    })
    .await
}

/// The exact value a public response's `Cache-Control` must carry with the
/// settings [`app_with_edge_cache_on`] uses.
const PUBLIC_CACHE_CONTROL: &str = "public, max-age=60, s-maxage=600";

/// Every tag header on a public response must carry `site`.
fn assert_public(response: &Response, tag_header: &str) -> Vec<String> {
    assert_eq!(
        header_str(response, header::CACHE_CONTROL.as_str()).as_deref(),
        Some(PUBLIC_CACHE_CONTROL),
        "expected a public response"
    );
    let vary = header_str(response, header::VARY.as_str()).unwrap_or_default();
    assert!(
        vary.split(',')
            .any(|v| v.trim().eq_ignore_ascii_case("cookie")),
        "Vary must include Cookie, got {vary:?}"
    );
    assert!(
        response.headers().get(header::SET_COOKIE).is_none(),
        "a public response never sets a cookie"
    );
    let tags = header_str(response, tag_header)
        .unwrap_or_else(|| panic!("a public response must carry the {tag_header} tag header"));
    let tags: Vec<String> = tags.split(',').map(|t| t.trim().to_string()).collect();
    assert!(
        tags.contains(&"site".to_string()),
        "every public page carries site: {tags:?}"
    );
    tags
}

/// A response that must never be shared: exactly `private, no-store`, and
/// never a tag header.
fn assert_private_no_store(response: &Response, context: &str) {
    assert_eq!(
        header_str(response, header::CACHE_CONTROL.as_str()).as_deref(),
        Some("private, no-store"),
        "{context} must be private, no-store"
    );
    assert!(
        header_str(response, "surrogate-key").is_none(),
        "{context} must carry no tag header"
    );
}

/// A response that must never be shared, but may answer with either
/// `private, no-store` or no `Cache-Control` at all (an anonymous error
/// response may take either, per the design).
fn assert_never_public(response: &Response, context: &str) {
    let cache_control = header_str(response, header::CACHE_CONTROL.as_str());
    assert_ne!(
        cache_control.as_deref(),
        Some(PUBLIC_CACHE_CONTROL),
        "{context} must never be public"
    );
    if let Some(cc) = &cache_control {
        assert!(
            !cc.contains("public"),
            "{context} must never carry public: {cc}"
        );
    }
    assert!(
        header_str(response, "surrogate-key").is_none(),
        "{context} must carry no tag header"
    );
}

// --- (a) An anonymous item page is public, with its tags, no Set-Cookie ---

#[test]
fn an_anonymous_item_page_is_public_with_site_and_item_tags() {
    run_test(async {
        let app = app_with_edge_cache_on().await;
        let id = create_published_item(&app, "B303 public item", 0).await;

        let response = get(&app, &prefixed_item_path(id)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let tags = assert_public(&response, "surrogate-key");
        assert!(
            tags.contains(&format!("item:{id}")),
            "the item it loaded: {tags:?}"
        );

        // HEAD gets the same treatment.
        let response = head(&app, &prefixed_item_path(id)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_public(&response, "surrogate-key");
    });
}

// --- (b) A gather listing carries `items` ---

#[test]
fn a_gather_listing_carries_the_items_tag() {
    run_test(async {
        // `it`/`he` (non-default languages) apply a translation overlay this
        // fixture has no `item_translation` table for (that table is owned by
        // the `trovato_content_translation` plugin, not installed here), so
        // this test reaches a `UrlPrefix` source without one: the *default*
        // language's own prefix, which needs `language_prefix_default` set
        // before the app is built (`AppState` snapshots it at construction,
        // same as the languages themselves). Reset after, so no other test in
        // the suite inherits it.
        seed_prefix_default(true).await;
        common::defer_cleanup(|| async {
            seed_prefix_default(false).await;
        });

        let app = app_with_edge_cache_on().await;
        // The gather access-count query (D-26) wants a real content type's
        // shape; `conference` is the tutorial fixture every other gather test
        // uses, with the two required date fields it needs to be created.
        app.ensure_conference_type().await;
        let id = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: "B303 gather listed item".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_start_date": "2026-01-01",
                        "field_end_date": "2026-01-02",
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("create conference item")
            .id;

        let query_id = format!("edge_cache_test_gather_{}", Uuid::now_v7().simple());
        app.state
            .gather()
            .register_query(trovato_kernel::gather::GatherQuery {
                query_id: query_id.clone(),
                label: "Edge cache test gather".to_string(),
                description: None,
                definition: trovato_kernel::gather::QueryDefinition {
                    base_table: "item".to_string(),
                    item_type: Some("conference".to_string()),
                    ..Default::default()
                },
                display: trovato_kernel::gather::QueryDisplay::default(),
                plugin: "core".to_string(),
                created: 1,
                changed: 1,
            })
            .await
            .expect("register the test gather query");

        let response = get(&app, &format!("/en/gather/{query_id}")).await;
        if response.status() != StatusCode::OK {
            let status = response.status();
            let body = body_string(response).await;
            panic!("expected 200, got {status}: {body}");
        }
        let tags = assert_public(&response, "surrogate-key");
        assert!(
            tags.contains(&"items".to_string()),
            "a gather run: {tags:?}"
        );
        // The listed item's own id is not required on a listing page (only
        // the pages that load a single item record it), but the listing
        // must not accidentally miss `items` because something else ran
        // first.
        let _ = id;
    });
}

// --- (c) Personal, token-bearing, mutating and error responses are never public ---

#[test]
fn a_signed_in_session_cookie_is_always_private_no_store() {
    run_test(async {
        let app = app_with_edge_cache_on().await;
        let id = create_published_item(&app, "B303 signed in item", 0).await;
        let name = username("b303signedin");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple-1",
                &format!("{name}@test.local"),
            )
            .await;

        let response = get_with_cookies(&app, &prefixed_item_path(id), &cookies).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_private_no_store(&response, "a signed-in GET of a published item page");
    });
}

#[test]
fn an_authorization_header_is_always_private_no_store() {
    run_test(async {
        let app = app_with_edge_cache_on().await;
        let id = create_published_item(&app, "B303 bearer item", 0).await;

        let response = app
            .request(
                Request::get(prefixed_item_path(id))
                    .header(header::AUTHORIZATION, "Bearer not-a-real-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_private_no_store(&response, "a GET carrying an Authorization header");
    });
}

#[test]
fn an_anonymous_login_page_sets_a_cookie_and_is_private_no_store() {
    run_test(async {
        let app = app_with_edge_cache_on().await;

        let response = get(&app, "/user/login").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(header::SET_COOKIE).is_some(),
            "the login page mints a CSRF token for an anonymous visitor, which writes the session"
        );
        assert_private_no_store(&response, "the anonymous login page");
    });
}

#[test]
fn a_post_is_always_private_no_store() {
    run_test(async {
        let app = app_with_edge_cache_on().await;
        let id = create_published_item(&app, "B303 post item", 0).await;

        let response = app
            .request(
                Request::post(prefixed_item_path(id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_private_no_store(&response, "a POST");
    });
}

#[test]
fn a_404_is_never_public() {
    run_test(async {
        let app = app_with_edge_cache_on().await;
        let missing = Uuid::now_v7();

        let response = get(&app, &prefixed_item_path(missing)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_never_public(&response, "an anonymous 404");
    });
}

// --- (d) Settings unset: nothing is ever public ---

#[test]
fn with_settings_unset_the_anonymous_page_carries_no_public_and_the_signed_in_one_is_private() {
    run_test(async {
        let app = TestApp::with_config(small_pool).await;
        let id = create_published_item(&app, "B303 unset settings item", 0).await;

        let response = get(&app, &prefixed_item_path(id)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_never_public(&response, "anonymous, EDGE_CACHE_S_MAXAGE unset");

        let name = username("b303unset");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple-2",
                &format!("{name}@test.local"),
            )
            .await;
        let response = get_with_cookies(&app, &prefixed_item_path(id), &cookies).await;
        assert_private_no_store(&response, "signed in, EDGE_CACHE_S_MAXAGE unset");
    });
}

// --- (e) A configured tag header name is used, comma separated ---

#[test]
fn a_configured_tag_header_name_is_comma_separated() {
    run_test(async {
        let app = TestApp::with_config(|c| {
            small_pool(c);
            c.runtime.edge_cache.max_age = 60;
            c.runtime.edge_cache.s_maxage = 600;
            c.runtime.edge_cache.tag_header = "Cache-Tag".to_string();
        })
        .await;
        let id = create_published_item(&app, "B303 cache-tag item", 0).await;

        let response = get(&app, &prefixed_item_path(id)).await;
        assert!(
            response.headers().get("surrogate-key").is_none(),
            "the default header name must not also be sent"
        );
        let tags = assert_public(&response, "cache-tag");
        assert!(tags.contains(&format!("item:{id}")));
        assert!(tags.len() >= 2, "comma separated: {tags:?}");
    });
}

// --- (f) A multi-language site: unprefixed is never public, prefixed is ---

#[test]
fn an_unprefixed_page_on_a_multi_language_site_is_never_public_but_its_prefixed_address_is() {
    run_test(async {
        let app = app_with_edge_cache_on().await;
        let id = create_published_item(&app, "B303 language item", 0).await;

        // This fixture always seeds `it` and `he` alongside `en` (see the
        // module docs): every app built here already is the multi-language
        // site problem 6 describes.
        let unprefixed = get(&app, &unprefixed_item_path(id)).await;
        assert_eq!(unprefixed.status(), StatusCode::OK);
        assert_never_public(&unprefixed, "unprefixed, on a multi-language site");

        let prefixed = get(&app, &prefixed_item_path(id)).await;
        assert_eq!(prefixed.status(), StatusCode::OK);
        assert_public(&prefixed, "surrogate-key");
    });
}

// --- (g) A route that sets its own Cache-Control keeps it ---

#[test]
fn a_route_that_sets_its_own_cache_control_is_left_alone() {
    run_test(async {
        let app = app_with_edge_cache_on().await;

        // /health sets its own Cache-Control: no-store (routes/health.rs).
        let response = get(&app, "/health").await;
        assert_eq!(response.status(), StatusCode::OK);
        let cache_control = header_str(&response, header::CACHE_CONTROL.as_str());
        assert_eq!(
            cache_control.as_deref(),
            Some("no-store"),
            "the route's own header, not edge_cache's, must be what is sent"
        );
        assert!(
            response.headers().get("surrogate-key").is_none(),
            "edge_cache adds no tag header to a response it left alone"
        );
    });
}

// --- (h) trovato_cdn_purge_http delivers a real purge; (i) its SSRF fence ---

/// Repo `plugins/` directory (two levels up from this crate), the pattern
/// `ai_send_path_test.rs` uses: tests run from `crates/kernel`, so the
/// default `./plugins` resolves to nothing.
fn plugins_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// A loopback server that records every POST body it receives.
async fn recording_server() -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
    let bodies: std::sync::Arc<Mutex<Vec<String>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
    let captured = std::sync::Arc::clone(&bodies);
    let handler = move |bytes: axum::body::Bytes| {
        let captured = std::sync::Arc::clone(&captured);
        async move {
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(String::from_utf8_lossy(&bytes).to_string());
            StatusCode::OK
        }
    };
    let router = axum::Router::new().route("/", axum::routing::post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), bodies)
}

/// Load `trovato_cdn_purge_http` into a dispatcher of its own, the pattern
/// `ai_send_path_test.rs` and `plugin_queue_v2_test.rs` use for a plugin a
/// `TestApp` does not load itself (its `plugins_dirs` normally points nowhere,
/// see `plugins_dir`'s docs, and a fresh test database starts with no
/// `plugin_status` rows either way).
fn load_purge_http_dispatcher() -> std::sync::Arc<trovato_kernel::tap::TapDispatcher> {
    let mut runtime = trovato_kernel::plugin::PluginRuntime::new(
        &trovato_kernel::plugin::PluginConfig::default(),
    )
    .expect("create runtime");
    runtime
        .load_plugin(&plugins_dir().join("trovato_cdn_purge_http"))
        .expect("load trovato_cdn_purge_http (build its wasm first)");
    let runtime = std::sync::Arc::new(runtime);
    let registry = std::sync::Arc::new(trovato_kernel::tap::TapRegistry::from_plugins(&runtime));
    std::sync::Arc::new(trovato_kernel::tap::TapDispatcher::new(runtime, registry))
}

#[test]
fn trovato_cdn_purge_http_delivers_item_tile_and_everything_purges() {
    run_test(async {
        let (mock_url, bodies) = recording_server().await;
        trovato_kernel::host::allow_loopback_http_for_tests(&mock_url);
        let dispatcher = load_purge_http_dispatcher();

        let app = TestApp::with_config(small_pool).await;
        trovato_kernel::models::SiteConfig::set(
            &app.db,
            "plugin.trovato_cdn_purge_http.url",
            serde_json::json!(mock_url),
        )
        .await
        .expect("configure the purge plugin's url");

        let redis =
            redis::Client::open(trovato_test_utils::env::redis_url()).expect("redis client");
        let mut cron = trovato_kernel::cron::CronService::new(redis, app.db.clone());
        cron.set_tap_dispatcher(dispatcher.clone());

        // Saving an item: the purge names the item and `items`.
        let id = create_published_item(&app, "B303 purge-on-save item", 0).await;
        let outcome = trovato_kernel::edge_cache::drain_now_with(&dispatcher, &app.db).await;
        assert!(
            matches!(
                outcome,
                trovato_kernel::edge_cache::DrainOutcome::Dispatched { .. }
            ),
            "expected a dispatch, got {outcome:?}"
        );
        let stats = cron
            .drain_plugin_queues()
            .await
            .expect("drain the plugin queue");
        assert!(
            stats.succeeded >= 1,
            "the purge job should have succeeded: {stats:?}"
        );

        {
            let bodies = bodies.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(bodies.len(), 1, "exactly one purge POST for the save");
            let sent: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
            let tags: Vec<String> = sent["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            assert!(tags.contains(&format!("item:{id}")), "{tags:?}");
            assert!(tags.contains(&"items".to_string()), "{tags:?}");
            assert_eq!(sent["all"], false);
        }
        bodies.lock().unwrap_or_else(|e| e.into_inner()).clear();

        // Updating a tile: the purge names `site`.
        let tile = app
            .state
            .tiles()
            .create(trovato_kernel::models::tile::CreateTile {
                machine_name: format!("b303_tile_{}", Uuid::now_v7().simple()),
                label: "B303 test tile".to_string(),
                region: Some("sidebar".to_string()),
                tile_type: None,
                config: Some(serde_json::json!({"content": "before"})),
                visibility: None,
                weight: None,
                status: None,
                plugin: None,
                stage_id: None,
            })
            .await
            .expect("create tile");
        app.state
            .tiles()
            .update(
                tile.id,
                trovato_kernel::models::tile::UpdateTile {
                    label: None,
                    region: None,
                    tile_type: None,
                    config: Some(serde_json::json!({"content": "after"})),
                    visibility: None,
                    weight: None,
                    status: None,
                    plugin: None,
                    stage_id: None,
                },
            )
            .await
            .expect("update tile")
            .expect("tile exists");
        trovato_kernel::edge_cache::drain_now_with(&dispatcher, &app.db).await;
        cron.drain_plugin_queues()
            .await
            .expect("drain the plugin queue");
        {
            let bodies = bodies.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(
                bodies.len(),
                1,
                "exactly one purge POST for the tile update"
            );
            let sent: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
            let tags: Vec<String> = sent["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            assert!(tags.contains(&"site".to_string()), "{tags:?}");
        }
        bodies.lock().unwrap_or_else(|e| e.into_inner()).clear();

        // The admin's "purge everything" delivers `"all": true`.
        trovato_kernel::edge_cache::purge_everything();
        trovato_kernel::edge_cache::drain_now_with(&dispatcher, &app.db).await;
        cron.drain_plugin_queues()
            .await
            .expect("drain the plugin queue");
        let bodies = bodies.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            bodies.len(),
            1,
            "exactly one purge POST for purge-everything"
        );
        let sent: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(sent["all"], true);
    });
}

/// (i) Without the test seam's explicit allowance, plugin HTTP to a loopback
/// address is still refused, exactly as any other private address is.
#[test]
fn plugin_http_to_an_unallowed_loopback_address_is_still_refused() {
    run_test(async {
        let (mock_url, bodies) = recording_server().await;
        // Deliberately not calling `allow_loopback_http_for_tests`.

        let mut runtime = trovato_kernel::plugin::PluginRuntime::new(
            &trovato_kernel::plugin::PluginConfig::default(),
        )
        .expect("create runtime");
        runtime
            .load_plugin(&plugins_dir().join("trovato_cdn_purge_http"))
            .expect("load trovato_cdn_purge_http (build its wasm first)");
        let runtime = std::sync::Arc::new(runtime);
        let registry =
            std::sync::Arc::new(trovato_kernel::tap::TapRegistry::from_plugins(&runtime));
        let dispatcher = trovato_kernel::tap::TapDispatcher::new(runtime, registry);

        trovato_test_utils::env::load_dotenv();
        let pool = sqlx::PgPool::connect(&trovato_test_utils::env::database_url())
            .await
            .expect("connect");
        trovato_kernel::db::run_migrations(&pool)
            .await
            .expect("migrate");
        trovato_kernel::models::SiteConfig::set(
            &pool,
            "plugin.trovato_cdn_purge_http.url",
            serde_json::json!(mock_url),
        )
        .await
        .expect("configure url");

        let services = trovato_kernel::tap::RequestServices::for_background(
            pool,
            None,
            None,
            reqwest::Client::new(),
        )
        .with_plugin_runtime(dispatcher.runtime().clone());
        let state = trovato_kernel::tap::RequestState::new(
            trovato_kernel::tap::UserContext::background(),
            services,
        );
        let job = serde_json::json!({"tags": ["site"], "all": false}).to_string();
        let outcome = dispatcher
            .dispatch_to_plugin_outcome("tap_queue_worker", &job, "trovato_cdn_purge_http", state)
            .await;

        assert_eq!(
            bodies.lock().unwrap_or_else(|e| e.into_inner()).len(),
            0,
            "the fence must refuse the request before it reaches the server"
        );
        assert!(
            matches!(outcome, trovato_kernel::tap::DispatchOutcome::Failed),
            "the plugin's own http_request call should have failed the whole tap: {outcome:?}"
        );
    });
}
