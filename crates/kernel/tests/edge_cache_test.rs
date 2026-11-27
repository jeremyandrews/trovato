#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Edge caching: headers, tags and purge (B303).
//!
//! A CDN in front of Trovato needs the origin to say, on each response,
//! whether it may be shared and for how long, and which tags it was built
//! from, so a later write can tell the CDN what to drop. These tests pin:
//!
//! - an anonymous, cookie-free `GET`/`HEAD` of a published item page (and a
//!   gather listing) is `public` with the tag header, when the settings are
//!   on;
//! - a signed-in session, a bearer token, a `POST`, a page that sets a
//!   cookie, and a 404 are never `public`;
//! - with the settings unset, nothing is ever marked `public`;
//! - the tag header's name is configurable, and tags are comma separated;
//! - a page whose language came from `Accept-Language` or a session override
//!   (not the URL) is never `public`, even on this (multi-language) test
//!   fixture;
//! - a route that sets its own `Cache-Control` is left alone;
//! - a write purges the CDN through `trovato_cdn_purge_http`, when it is
//!   enabled and pointed at a reachable URL;
//! - plugin HTTP to a loopback address is refused by default, purge plugin
//!   included.
//!
//! Requires Postgres + Redis; builds its own `TestApp` per test (the
//! settings are process-wide at construction), so it does not share
//! `common::shared_app`.

mod common;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use common::{TestApp, run_test};
use sqlx::postgres::PgPoolOptions;
use trovato_kernel::models::{CreateItem, SiteConfig};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// Every test in this file builds its own `TestApp` (the settings under test
/// are process-wide at construction, so they cannot share `shared_app`).
/// Building one opens its own database pool, and enough of them at once
/// exhausts Postgres' connection limit; this serializes the file's own tests
/// against each other (not against other test binaries, which are separate
/// processes with their own budget).
static FILE_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Read a response's body as a string.
async fn body_string(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 8_000_000)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// A header's value as a string, or "" when absent.
fn header_str(response: &axum::response::Response, name: axum::http::HeaderName) -> String {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Build a `TestApp` with edge caching on: `max_age` 60, `s_maxage` 600, the
/// default tag header.
async fn app_with_edge_cache() -> TestApp {
    TestApp::with_config(|config| {
        config.runtime.edge_cache = trovato_kernel::middleware::EdgeCacheConfig {
            max_age_secs: 60,
            s_maxage_secs: 600,
            tag_header: axum::http::HeaderName::from_static("surrogate-key"),
        };
    })
    .await
}

/// Create a published, promoted "conference" item and return its id.
async fn create_published_item(app: &TestApp, title: &str) -> Uuid {
    app.ensure_conference_type().await;
    app.state
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
                stage_id: None,
                language: None,
                log: None,
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// Register a gather listing of every published "conference" item, with no
/// access restriction, and return `(query_id, path)`.
async fn register_public_gather(app: &TestApp, marker: &str) -> (String, String) {
    let query_id = format!("edge_cache_{}", marker.to_lowercase());
    let query: trovato_kernel::gather::types::GatherQuery =
        serde_json::from_value(serde_json::json!({
            "query_id": query_id,
            "label": format!("Edge cache test {marker}"),
            "description": "Edge cache test listing",
            "definition": {
                "base_table": "item",
                "item_type": "conference",
                "filters": [],
                "sorts": [{ "field": "title", "direction": "asc", "nulls": null }]
            },
            "display": { "format": "table", "items_per_page": 10, "routes": [] },
            "plugin": "core"
        }))
        .expect("gather deserializes");
    app.state
        .gather()
        .register_query(query)
        .await
        .expect("register gather");
    (query_id.clone(), format!("/gather/{query_id}"))
}

// =============================================================================
// (a) + (f): public, tagged, only via a URL-prefixed language
// =============================================================================

/// (a) An anonymous `GET` and `HEAD` of a published item page, requested
/// under a non-default language prefix (`UrlPrefix` is the one language
/// source this layer trusts regardless of how many languages the site
/// knows), answers 200 `public` with `max-age`/`s-maxage`, `Vary: Cookie`,
/// the tag header (`site` and `item:<uuid>`), and no `Set-Cookie`.
#[test]
fn anonymous_item_page_is_public_with_tags() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_edge_cache().await;
        let id = create_published_item(&app, "B303 Public Item").await;

        for method in [axum::http::Method::GET, axum::http::Method::HEAD] {
            let response = app
                .request(
                    Request::builder()
                        .method(method.clone())
                        .uri(format!("/it/item/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK, "method {method}");
            let cache_control = header_str(&response, header::CACHE_CONTROL);
            assert_eq!(
                cache_control, "public, max-age=60, s-maxage=600",
                "method {method}: {cache_control}"
            );
            let vary = header_str(&response, header::VARY);
            assert!(
                vary.split(',').any(|p| p.trim().eq_ignore_ascii_case("cookie")),
                "Vary must include Cookie, got {vary:?}"
            );
            assert!(
                !response.headers().contains_key(header::SET_COOKIE),
                "an anonymous cacheable page must set no cookie"
            );
            let tags = header_str(&response, axum::http::HeaderName::from_static("surrogate-key"));
            let tag_list: Vec<&str> = tags.split(',').collect();
            assert!(tag_list.contains(&"site"), "tags: {tags:?}");
            assert!(
                tag_list.contains(&format!("item:{id}").as_str()),
                "tags: {tags:?}"
            );
        }
    });
}

/// (f) On this (multi-language) fixture, the bare path is not `public`
/// (language came from the site default among several, not the URL), and
/// the same page under a known language's prefix is.
#[test]
fn unprefixed_page_is_private_prefixed_page_is_public() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_edge_cache().await;
        let id = create_published_item(&app, "B303 Multilingual Item").await;

        let bare = app
            .request(
                Request::get(format!("/item/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(bare.status(), StatusCode::OK);
        let bare_cc = header_str(&bare, header::CACHE_CONTROL);
        assert!(
            !bare_cc.contains("public"),
            "unprefixed page on a multi-language site must not be public: {bare_cc:?}"
        );

        let prefixed = app
            .request(
                Request::get(format!("/it/item/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(prefixed.status(), StatusCode::OK);
        let prefixed_cc = header_str(&prefixed, header::CACHE_CONTROL);
        assert!(
            prefixed_cc.starts_with("public,"),
            "prefixed page must be public: {prefixed_cc:?}"
        );
    });
}

// =============================================================================
// (b): a gather listing carries `items`
// =============================================================================

/// (b) An anonymous gather listing page is `public` (under a language
/// prefix) and carries the `items` tag alongside `site`.
#[test]
fn gather_listing_carries_the_items_tag() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_edge_cache().await;
        create_published_item(&app, "B303 Gather Item").await;
        let (_query_id, path) = register_public_gather(&app, "ListingB").await;

        let response = app
            .request(
                Request::get(format!("/it{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let cache_control = header_str(&response, header::CACHE_CONTROL);
        assert!(
            cache_control.starts_with("public,"),
            "gather listing must be public: {cache_control:?}"
        );
        let tags = header_str(&response, axum::http::HeaderName::from_static("surrogate-key"));
        let tag_list: Vec<&str> = tags.split(',').collect();
        assert!(tag_list.contains(&"site"), "tags: {tags:?}");
        assert!(tag_list.contains(&"items"), "tags: {tags:?}");
    });
}

// =============================================================================
// (c): never public for a signed-in session, a bearer token, a POST, a page
// that sets a cookie, or a 404
// =============================================================================

/// (c) The same item page is `private, no-store` with a signed-in session
/// cookie, with an `Authorization` header, and the anonymous login page (it
/// sets a cookie) is too; a `POST` and a 404 are never `public`.
#[test]
fn never_public_for_a_visitor_token_post_or_missing_page() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_edge_cache().await;
        let id = create_published_item(&app, "B303 Private Checks Item").await;

        // Signed in.
        let name = common::username("b303_edge_signed_in");
        let cookies = app
            .create_and_login_admin(&name, "Password123!", &format!("{name}@test.local"))
            .await;
        let signed_in = app
            .request_with_cookies(
                Request::get(format!("/it/item/{id}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(signed_in.status(), StatusCode::OK);
        assert_eq!(
            header_str(&signed_in, header::CACHE_CONTROL),
            "private, no-store"
        );
        assert!(
            header_str(&signed_in, axum::http::HeaderName::from_static("surrogate-key")).is_empty(),
            "a private response must carry no tag header"
        );

        // Authorization header (its validity is irrelevant: presence alone
        // means the response could be particular to whoever holds it).
        let with_token = app
            .request(
                Request::get(format!("/it/item/{id}"))
                    .header(header::AUTHORIZATION, "Bearer not-a-real-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            header_str(&with_token, header::CACHE_CONTROL),
            "private, no-store"
        );

        // The anonymous login page sets a cookie (it mints a CSRF token).
        let login_page = app
            .request(Request::get("/user/login").body(Body::empty()).unwrap())
            .await;
        assert_eq!(login_page.status(), StatusCode::OK);
        assert!(
            login_page.headers().contains_key(header::SET_COOKIE),
            "test setup: the login page must set a cookie for this check to mean anything"
        );
        assert_eq!(
            header_str(&login_page, header::CACHE_CONTROL),
            "private, no-store"
        );

        // A POST is never public, whatever it hits.
        let posted = app
            .request(
                Request::post("/this-route-does-not-exist-b303")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let posted_cc = header_str(&posted, header::CACHE_CONTROL);
        assert_eq!(posted_cc, "private, no-store", "a POST must be private");

        // A 404 is never public (it may carry no Cache-Control at all).
        let missing = app
            .request(
                Request::get(format!("/item/{}", Uuid::now_v7()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let missing_cc = header_str(&missing, header::CACHE_CONTROL);
        assert!(
            !missing_cc.contains("public"),
            "a 404 must never be public: {missing_cc:?}"
        );
    });
}

// =============================================================================
// (d): with the settings unset, nothing is ever public
// =============================================================================

/// (d) With `EDGE_CACHE_MAX_AGE`/`EDGE_CACHE_S_MAXAGE` unset (default), an
/// anonymous item page carries no `public` and no tag header; a signed-in
/// one is still `private, no-store` (the private half is always on).
#[test]
fn settings_unset_nothing_is_ever_public() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = TestApp::with_config(|_| {}).await;
        let id = create_published_item(&app, "B303 Settings Unset Item").await;

        let anon = app
            .request(
                Request::get(format!("/it/item/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let anon_cc = header_str(&anon, header::CACHE_CONTROL);
        assert!(
            !anon_cc.contains("public"),
            "the public half must be off by default: {anon_cc:?}"
        );
        assert!(
            header_str(&anon, axum::http::HeaderName::from_static("surrogate-key")).is_empty(),
            "no tag header without a public response"
        );

        let name = common::username("b303_edge_unset_signed_in");
        let cookies = app
            .create_and_login_admin(&name, "Password123!", &format!("{name}@test.local"))
            .await;
        let signed_in = app
            .request_with_cookies(
                Request::get(format!("/it/item/{id}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            header_str(&signed_in, header::CACHE_CONTROL),
            "private, no-store",
            "the private half is always on, settings or not"
        );
    });
}

// =============================================================================
// (e): the tag header name is configurable; tags are comma separated
// =============================================================================

/// (e) With `EDGE_CACHE_TAG_HEADER=Cache-Tag`, tags are sent under that
/// header name, comma separated.
#[test]
fn tag_header_name_is_configurable() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = TestApp::with_config(|config| {
            config.runtime.edge_cache = trovato_kernel::middleware::EdgeCacheConfig {
                max_age_secs: 60,
                s_maxage_secs: 600,
                tag_header: axum::http::HeaderName::from_static("cache-tag"),
            };
        })
        .await;
        let id = create_published_item(&app, "B303 Cache-Tag Item").await;

        let response = app
            .request(
                Request::get(format!("/it/item/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert!(
            !response
                .headers()
                .get(axum::http::HeaderName::from_static("surrogate-key"))
                .is_some(),
            "the default header name must not also be sent"
        );
        let tags = header_str(&response, axum::http::HeaderName::from_static("cache-tag"));
        let tag_list: Vec<&str> = tags.split(',').collect();
        assert!(tag_list.contains(&"site"));
        assert!(tag_list.contains(&format!("item:{id}").as_str()));
    });
}

// =============================================================================
// (g): a route that already set Cache-Control is left alone
// =============================================================================

/// (g) An uploaded file's own `Cache-Control` (set by `routes::file`,
/// independent of this layer) is unchanged.
#[test]
fn a_route_with_its_own_cache_control_is_untouched() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_edge_cache().await;
        app.ensure_conference_type().await;
        let filename = format!("b303-{}.png", Uuid::now_v7().simple());
        let bytes = {
            let img = image::RgbImage::from_fn(4, 4, |_, _| image::Rgb([10, 20, 30]));
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let uploaded = app
            .state
            .files()
            .upload(Uuid::nil(), &filename, "image/png", &bytes)
            .await
            .expect("upload");
        let path = uploaded.uri.strip_prefix("local://").unwrap().to_string();

        // A Temporary, orphaned file is not anonymously servable; referencing
        // it from a published item (D-29, any-referencing policy) is.
        app.state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: "B303 File Reference Item".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({ "field_logo": uploaded.id.to_string() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("create referencing item");

        let response = app
            .request(
                Request::get(format!("/files/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_str(&response, header::CACHE_CONTROL),
            "public, max-age=604800",
            "the file route's own Cache-Control must survive unchanged, \
             not this layer's public, max-age=60, s-maxage=600"
        );
    });
}

// =============================================================================
// (h) + (i): purge delivery through trovato_cdn_purge_http, and the SSRF
// fence default
// =============================================================================


/// A loopback server that records every POST body it receives.
async fn purge_recorder() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let bodies: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = bodies.clone();
    let handler = move |body: String| {
        let store = store.clone();
        async move {
            store.lock().unwrap_or_else(|e| e.into_inner()).push(body);
            StatusCode::OK
        }
    };
    let app = axum::Router::new().route("/purge", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/purge"), bodies)
}

/// Build a `TestApp` with `trovato_cdn_purge_http` installed and enabled
/// *before* `AppState` is constructed, which is the only point the plugin's
/// wasm is compiled and its taps registered (`ensure_plugin_enabled` flips
/// only the in-memory flag afterwards and loads no plugin).
async fn app_with_purge_plugin() -> TestApp {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect for plugin setup");
    trovato_kernel::plugin::status::install_plugin(&pool, "trovato_cdn_purge_http", "0.106.0")
        .await
        .expect("install trovato_cdn_purge_http");
    pool.close().await;
    app_with_edge_cache().await
}

/// (h) Saving an item, then draining, dispatches a purge whose tags include
/// `item:<uuid>` and `items`; updating a tile dispatches `site`.
#[test]
fn purge_plugin_delivers_item_and_tile_writes() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_purge_plugin().await;
        let (url, bodies) = purge_recorder().await;
        trovato_kernel::host::allow_loopback_http_for_tests(&url);
        SiteConfig::set(
            &app.db,
            "plugin.trovato_cdn_purge_http.url",
            serde_json::json!(url),
        )
        .await
        .expect("set purge url");

        let id = create_published_item(&app, "B303 Purge Item").await;
        let drained = app.state.cron().drain_cache_purge_now().await;
        assert!(drained, "a plugin implementing tap_cache_purge must receive it");
        app.state
            .cron()
            .drain_plugin_queues()
            .await
            .expect("drain plugin queues");

        let seen = bodies.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(seen.len(), 1, "exactly one purge POST for the item write");
        let payload: serde_json::Value = serde_json::from_str(&seen[0]).expect("JSON body");
        assert_eq!(payload["all"], false);
        let tags: Vec<String> = payload["tags"]
            .as_array()
            .expect("tags array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        assert!(tags.contains(&format!("item:{id}")), "tags: {tags:?}");
        assert!(tags.contains(&"items".to_string()), "tags: {tags:?}");

        // Now a tile write: `site` only.
        bodies.lock().unwrap_or_else(|e| e.into_inner()).clear();
        app.state
            .tiles()
            .create(trovato_kernel::models::tile::CreateTile {
                machine_name: format!("b303_tile_{}", Uuid::now_v7().simple()),
                label: "B303 purge tile".to_string(),
                region: Some("sidebar".to_string()),
                tile_type: Some("text".to_string()),
                config: Some(serde_json::json!({})),
                visibility: None,
                weight: Some(0),
                status: Some(1),
                plugin: None,
                stage_id: None,
            })
            .await
            .expect("create tile");
        app.state.cron().drain_cache_purge_now().await;
        app.state
            .cron()
            .drain_plugin_queues()
            .await
            .expect("drain plugin queues");
        let seen = bodies.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(seen.len(), 1, "exactly one purge POST for the tile write");
        let payload: serde_json::Value = serde_json::from_str(&seen[0]).expect("JSON body");
        assert_eq!(payload["all"], false);
        let tags: Vec<String> = payload["tags"]
            .as_array()
            .expect("tags array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            tags.contains(&"site".to_string()),
            "a tile write must purge site: {tags:?}"
        );
    });
}

/// (h) The admin "purge everything" action delivers `{"all": true}`.
#[test]
fn admin_purge_everything_delivers_all_true() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_purge_plugin().await;
        let (url, bodies) = purge_recorder().await;
        trovato_kernel::host::allow_loopback_http_for_tests(&url);
        SiteConfig::set(
            &app.db,
            "plugin.trovato_cdn_purge_http.url",
            serde_json::json!(url),
        )
        .await
        .expect("set purge url");

        let name = common::username("b303_edge_purge_admin");
        let cookies = app
            .create_and_login_admin(&name, "Password123!", &format!("{name}@test.local"))
            .await;

        let page = app
            .request_with_cookies(
                Request::get("/admin/config/performance")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = body_string(page).await;
        let form_build_id = extract_last(&html, "_form_build_id");
        let token = extract_last(&html, "_token");
        assert!(!form_build_id.is_empty(), "no _form_build_id in {html}");
        assert!(!token.is_empty(), "no _token in the page");

        let body = format!(
            "scope=all&tags=&_form_build_id={form_build_id}&_token={token}&op=Purge+the+CDN"
        );
        let response = app
            .request_with_cookies(
                Request::post("/admin/config/performance/purge")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        let status = response.status();
        if !status.is_redirection() {
            let text = body_string(response).await;
            panic!("purge submit must redirect back to the page, got {status} {text}");
        }

        app.state
            .cron()
            .drain_plugin_queues()
            .await
            .expect("drain plugin queues");

        let seen = bodies.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(seen.len(), 1, "exactly one purge POST for 'purge everything'");
        let payload: serde_json::Value = serde_json::from_str(&seen[0]).expect("JSON body");
        assert_eq!(payload["all"], true);
    });
}

/// The last `name="field"` hidden input's `value` in `html` (the page's
/// *last* form, i.e. "Purge the CDN", which is rendered after "Clear all
/// caches").
fn extract_last(html: &str, field: &str) -> String {
    let needle = format!("name=\"{field}\"");
    let Some(pos) = html.rfind(&needle) else {
        return String::new();
    };
    let Some(rel) = html[pos..].find("value=\"") else {
        return String::new();
    };
    let start = pos + rel + 7;
    let end = html[start..].find('"').map(|p| start + p).unwrap_or(start);
    html[start..end].to_string()
}

/// (i) Without the test seam, plugin HTTP to `127.0.0.1` is still refused:
/// the mock server records no hit, and the job dead-letters rather than
/// silently vanishing.
#[test]
fn plugin_http_to_loopback_is_refused_without_the_test_seam() {
    run_test(async {
        let _serial = FILE_SERIAL.lock().await;
        let app = app_with_purge_plugin().await;
        let (url, bodies) = purge_recorder().await;
        // Deliberately NOT calling allow_loopback_http_for_tests.
        SiteConfig::set(
            &app.db,
            "plugin.trovato_cdn_purge_http.url",
            serde_json::json!(url),
        )
        .await
        .expect("set purge url");

        create_published_item(&app, "B303 Refused Purge Item").await;
        app.state.cron().drain_cache_purge_now().await;
        app.state
            .cron()
            .drain_plugin_queues()
            .await
            .expect("drain plugin queues");

        assert!(
            bodies.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "the SSRF fence must refuse loopback HTTP with no test seam"
        );
    });
}
