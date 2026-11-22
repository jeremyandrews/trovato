#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A gather that declares a permission is served to nobody without it.
//!
//! `definition.access.permission` is a gate in front of the query: every path
//! that runs a registered gather checks it before executing, and a menu link to
//! such a gather is not drawn for a viewer who would be refused. Item-level
//! access still filters the rows of a gather the viewer may run; that is
//! `gather_access_test.rs`.
//!
//! The permission is declared through JSON here, the way a plugin migration or
//! a config import declares it, so the tests exercise the stored shape rather
//! than a Rust constructor.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use tower::ServiceExt;
use trovato_kernel::gather::types::GatherQuery;
use trovato_kernel::middleware::language::ResolvedLanguage;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_B560_0000_0001;

/// This file's own item type, so its items appear in no other file's listings.
const ITEM_TYPE: &str = "gather_perm_test";

/// The permission the protected gathers in this file declare.
const PERMISSION: &str = "view gather permission test";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

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
         VALUES ($1, 'Gather Permission Test', 'Fixture type', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed item type");
    tx.commit().await.expect("commit type seed");
    app.state
        .content_types()
        .replace(
            ITEM_TYPE,
            "Gather Permission Test",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

/// One published item of this file's type, deleted when the test ends.
async fn published_item(app: &TestApp, title: &str) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather permission test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;
    let state = app.state.clone();
    defer_cleanup(move || async move {
        state.items().delete(id, &admin()).await.ok();
    });
    id
}

/// Everything one test needs: a protected gather with a route alias and a feed,
/// a published row in it, and the names to look for.
struct Fixture {
    query: GatherQuery,
    title: String,
    alias_path: String,
    feed_path: String,
}

/// Register a gather over this file's type that declares [`PERMISSION`], as a
/// plugin migration would write it: a JSON definition and display.
async fn protected_gather(app: &TestApp, prefix: &str) -> Fixture {
    ensure_item_type(app).await;
    let stamp = Uuid::now_v7().simple().to_string();
    let query_id = format!("gperm_{prefix}_{stamp}");
    let title = format!("GPERM-{prefix}-{stamp}");
    let alias_path = format!("/gperm-{stamp}/list");
    let feed_path = format!("/rss/gperm-{stamp}.xml");

    let query: GatherQuery = serde_json::from_value(serde_json::json!({
        "query_id": query_id,
        "label": "Protected gather",
        "description": "Rows only a holder of the permission may read",
        "definition": {
            "base_table": "item",
            "item_type": ITEM_TYPE,
            "access": { "permission": PERMISSION },
            "sorts": [{ "field": "created", "direction": "desc", "nulls": null }]
        },
        "display": {
            "format": "table",
            "items_per_page": 10,
            "routes": [{ "path": alias_path, "params": [] }],
            "feed": { "path": feed_path, "title": "Protected feed", "items": 10 }
        },
        "plugin": "core"
    }))
    .expect("a gather declaring access.permission deserializes");

    app.state
        .gather()
        .register_query(query.clone())
        .await
        .expect("register protected gather");
    let pool = app.db.clone();
    let id = query.query_id.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(&id)
            .execute(&pool)
            .await;
    });

    published_item(app, &title).await;

    Fixture {
        query,
        title,
        alias_path,
        feed_path,
    }
}

async fn body_of(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// GET `path` through the application router as the holder of `cookies`
/// (empty for anonymous).
async fn get(app: &TestApp, path: &str, cookies: &str) -> (StatusCode, Option<String>, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (status, location, body_of(response).await)
}

/// GET `path` through a router built from `query` the way `main` builds the
/// route-alias and feed routers at startup. The shared `TestApp` router is
/// built before these gathers exist, so it does not carry their routes.
async fn get_startup_routes(
    app: &TestApp,
    query: &GatherQuery,
    path: &str,
    cookies: &str,
) -> (StatusCode, Option<String>, String) {
    let session_layer = trovato_kernel::session::create_session_layer(
        &trovato_test_utils::env::redis_url(),
        tower_sessions::cookie::SameSite::Strict,
        trovato_kernel::session::CookieSecure::Always,
    )
    .await
    .expect("create session layer");
    let queries = std::slice::from_ref(query);
    let router = trovato_kernel::routes::gather_routes::build_gather_route_router(queries)
        .merge(trovato_kernel::routes::feed::build_feed_router(queries))
        .with_state(app.state.clone())
        .layer(axum::Extension(ResolvedLanguage("en".to_string())))
        .layer(session_layer);

    let mut request = Request::get(path).body(Body::empty()).unwrap();
    if !cookies.is_empty() {
        request
            .headers_mut()
            .insert(header::COOKIE, cookies.parse().unwrap());
    }
    let response = router.oneshot(request).await.expect("serve request");
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (status, location, body_of(response).await)
}

fn assert_login_redirect(status: StatusCode, location: Option<&str>, body: &str, what: &str) {
    assert!(
        status.is_redirection(),
        "{what}: an anonymous visitor is sent to log in, got {status}: {body}"
    );
    assert_eq!(
        location,
        Some("/user/login"),
        "{what}: the redirect is the login page"
    );
}

// =============================================================================
// /gather/{query_id}
// =============================================================================

#[test]
fn gather_page_sends_anonymous_to_login_and_refuses_a_user_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "page").await;
        let path = format!("/gather/{}", fx.query.query_id);

        let (status, location, body) = get(app, &path, "").await;
        assert_login_redirect(status, location.as_deref(), &body, "anonymous /gather");
        assert!(!body.contains(&fx.title), "no row reaches anonymous");

        let (_, cookies) = user_holding(app, "gperm-none", &[]).await;
        let (status, _, body) = get(app, &path, &cookies).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "user without it: {body}");
        assert!(!body.contains(&fx.title), "no row reaches the user");
    });
}

#[test]
fn gather_page_serves_rows_to_the_holder_and_to_an_admin() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "holder").await;
        let path = format!("/gather/{}", fx.query.query_id);

        let (_, cookies) = user_holding(app, "gperm-holder", &[PERMISSION]).await;
        let (status, _, body) = get(app, &path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "holder: {body}");
        assert!(body.contains(&fx.title), "the holder sees the row");

        let name = common::username("gperm-admin");
        let cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let (status, _, body) = get(app, &path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "admin: {body}");
        assert!(body.contains(&fx.title), "the admin sees the row");
    });
}

// =============================================================================
// JSON API
// =============================================================================

#[test]
fn execute_api_refuses_without_the_permission_and_serves_the_holder() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "execute").await;
        let path = format!("/api/query/{}/execute", fx.query.query_id);

        let (status, _, body) = get(app, &path, "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous: {body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("JSON error body");
        assert!(json.get("message").is_some(), "JSON error: {body}");
        assert!(!body.contains(&fx.title));

        let (_, cookies) = user_holding(app, "gperm-exec-none", &[]).await;
        let (status, _, body) = get(app, &path, &cookies).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "user without it: {body}");

        let (_, cookies) = user_holding(app, "gperm-exec", &[PERMISSION]).await;
        let (status, _, body) = get(app, &path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "holder: {body}");
        assert!(body.contains(&fx.title), "the holder gets the row: {body}");
    });
}

#[test]
fn query_definition_api_refuses_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "definition").await;
        let path = format!("/api/query/{}", fx.query.query_id);

        let (status, _, body) = get(app, &path, "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous: {body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("JSON error body");
        assert!(json.get("message").is_some(), "JSON error: {body}");

        let (_, cookies) = user_holding(app, "gperm-def", &[PERMISSION]).await;
        let (status, _, body) = get(app, &path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "holder: {body}");
        assert!(body.contains(&fx.query.query_id));
    });
}

#[test]
fn query_listing_omits_a_gather_the_viewer_may_not_run() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "listing").await;
        let id = format!("\"{}\"", fx.query.query_id);

        let (status, _, body) = get(app, "/api/queries", "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains(&id), "anonymous listing names it: {body}");

        let (_, cookies) = user_holding(app, "gperm-list", &[PERMISSION]).await;
        let (status, _, body) = get(app, "/api/queries", &cookies).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&id), "the holder's listing names it");
    });
}

// =============================================================================
// Route alias and feed
// =============================================================================

#[test]
fn route_alias_refuses_without_the_permission_in_html_and_json() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "alias").await;

        let (status, location, body) = get_startup_routes(app, &fx.query, &fx.alias_path, "").await;
        assert_login_redirect(status, location.as_deref(), &body, "anonymous alias");
        assert!(!body.contains(&fx.title));

        let json_path = format!("{}?format=json", fx.alias_path);
        let (status, _, body) = get_startup_routes(app, &fx.query, &json_path, "").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "anonymous alias JSON: {body}"
        );
        assert!(!body.contains(&fx.title));
        let json: serde_json::Value = serde_json::from_str(&body).expect("JSON error body");
        assert!(json.get("message").is_some(), "JSON error: {body}");

        let (_, cookies) = user_holding(app, "gperm-alias-none", &[]).await;
        let (status, _, body) = get_startup_routes(app, &fx.query, &fx.alias_path, &cookies).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "user without it: {body}");

        let (_, cookies) = user_holding(app, "gperm-alias", &[PERMISSION]).await;
        let (status, _, body) = get_startup_routes(app, &fx.query, &fx.alias_path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "holder: {body}");
        assert!(body.contains(&fx.title), "the holder sees the row");
        let (status, _, body) = get_startup_routes(app, &fx.query, &json_path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "holder JSON: {body}");
        assert!(body.contains(&fx.title), "the holder gets the row");
    });
}

#[test]
fn feed_refuses_without_the_permission_and_serves_the_holder() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "feed").await;

        let (status, _, body) = get_startup_routes(app, &fx.query, &fx.feed_path, "").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "anonymous feed: {body}");
        assert!(!body.contains(&fx.title));

        let (_, cookies) = user_holding(app, "gperm-feed", &[PERMISSION]).await;
        let (status, _, body) = get_startup_routes(app, &fx.query, &fx.feed_path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "holder feed: {body}");
        assert!(
            body.contains(&fx.title),
            "the holder's feed carries the row"
        );
    });
}

// =============================================================================
// Menu links
// =============================================================================

/// Insert a footer `menu_link`, removed when the test ends. The footer, not the
/// main menu: a main-menu row replaces every plugin's navigation on every page,
/// which would reach into tests running beside this one.
async fn footer_link(app: &TestApp, path: &str, title: &str, parent: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO menu_link (id, menu_name, path, title, parent_id, weight, hidden, plugin, \
         stage_id, created, changed) VALUES ($1, 'footer', $2, $3, $4, 0, false, 'core', $5, 0, 0)",
    )
    .bind(id)
    .bind(path)
    .bind(title)
    .bind(parent)
    .bind(LIVE_STAGE_ID)
    .execute(&app.db)
    .await
    .expect("insert footer link");
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM menu_link WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

#[test]
fn menu_links_to_a_protected_gather_are_drawn_only_for_a_viewer_who_may_follow_them() {
    run_test(async {
        let app = shared_app().await;
        let fx = protected_gather(app, "menu").await;
        let stamp = Uuid::now_v7().simple().to_string();

        // A URL alias standing in for the gather, as Netgrasp's `/devices` does.
        let url_alias = format!("/gperm-alias-{stamp}");
        sqlx::query(
            "INSERT INTO url_alias (id, source, alias, language, stage_id, created) \
             VALUES ($1, $2, $3, 'en', $4, 0)",
        )
        .bind(Uuid::now_v7())
        .bind(format!("/gather/{}", fx.query.query_id))
        .bind(&url_alias)
        .bind(LIVE_STAGE_ID)
        .execute(&app.db)
        .await
        .expect("insert url alias");
        let pool = app.db.clone();
        let alias_for_cleanup = url_alias.clone();
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM url_alias WHERE alias = $1")
                .bind(&alias_for_cleanup)
                .execute(&pool)
                .await;
        });

        let direct = format!("GPERM-direct-{stamp}");
        let routed = format!("GPERM-routed-{stamp}");
        let aliased = format!("GPERM-aliased-{stamp}");
        let child = format!("GPERM-child-{stamp}");
        let public = format!("GPERM-public-{stamp}");

        let parent = footer_link(
            app,
            &format!("/gather/{}", fx.query.query_id),
            &direct,
            None,
        )
        .await;
        footer_link(app, &fx.alias_path, &routed, None).await;
        footer_link(app, &url_alias, &aliased, None).await;
        footer_link(app, &format!("/gperm-child-{stamp}"), &child, Some(parent)).await;
        footer_link(app, &format!("/gperm-public-{stamp}"), &public, None).await;

        let (status, _, body) = get(app, "/user/login", "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&public),
            "a public link is drawn for anonymous"
        );
        for hidden in [&direct, &routed, &aliased, &child] {
            assert!(
                !body.contains(hidden.as_str()),
                "{hidden} is drawn for an anonymous visitor who would be refused"
            );
        }

        // The holder's own view of the gather page draws the same footer.
        let (_, cookies) = user_holding(app, "gperm-menu", &[PERMISSION]).await;
        let (status, _, body) = get(app, &format!("/gather/{}", fx.query.query_id), &cookies).await;
        assert_eq!(status, StatusCode::OK, "holder: {body}");
        for shown in [&direct, &routed, &aliased, &child, &public] {
            assert!(
                body.contains(shown.as_str()),
                "{shown} is drawn for a viewer holding the permission"
            );
        }
    });
}
