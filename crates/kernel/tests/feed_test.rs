#![allow(clippy::unwrap_used, clippy::expect_used)]
//! RSS feeds served from gather queries.
//!
//! The unit tests in `routes::feed` cover registration rules and XML rendering.
//! What is pinned here is that a declared feed is actually served, with real
//! content from a real query execution — the defect being that
//! `trovato_feeds` declared page-style routes with callbacks and no `tap_api`,
//! so `/rss/insights.xml` and `/rss/planet-drupal.xml` 404ed and its RSS
//! builders were dead code.
//!
//! The access test is the one that matters most for the design: a feed is an
//! execution of the query as the fetching viewer, so an unpublished item must
//! not appear in the anonymous feed.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use tower::ServiceExt;
use trovato_kernel::gather::types::{
    GatherFeed, GatherQuery, QueryDefinition, QueryDisplay, QuerySort, SortDirection,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateUrlAlias, UrlAlias};
use trovato_kernel::routes::feed::{build_feed_router, feed_links};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_F403_0000_0001;

/// This file's own item type, so its items cannot appear in another file's
/// listings and vice versa.
const ITEM_TYPE: &str = "feed_test";

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
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
         VALUES ($1, 'Feed Test', 'Fixture type for feed tests', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed feed test item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .create(
            ITEM_TYPE,
            "Feed Test",
            Some("Fixture type for feed tests"),
            settings,
        )
        .await
        .ok();
}

/// A gather query over this file's item type, newest first, with a feed at
/// `path`. The query id is unique per call so concurrent runs cannot collide on
/// the `gather_query` row.
fn feed_query(query_id: &str, path: &str) -> GatherQuery {
    GatherQuery {
        query_id: query_id.to_string(),
        label: "Feed Test Query".to_string(),
        description: Some("Items for the feed test".to_string()),
        definition: QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some(ITEM_TYPE.to_string()),
            sorts: vec![QuerySort {
                field: "created".to_string(),
                direction: SortDirection::Desc,
                nulls: None,
            }],
            ..Default::default()
        },
        display: QueryDisplay {
            items_per_page: 10,
            feed: Some(GatherFeed {
                path: path.to_string(),
                title: Some("Feed Test".to_string()),
                description: Some("What the feed is about".to_string()),
                items: 20,
            }),
            ..Default::default()
        },
        plugin: "core".to_string(),
        created: 0,
        changed: 0,
    }
}

/// Register the query with the gather service, which is where the feed handler
/// looks it up. Registration also persists the display config, so this covers
/// the `feed` block surviving a serialize/deserialize round trip through the
/// `gather_query` row.
async fn register(app: &TestApp, query: &GatherQuery) {
    app.state
        .gather()
        .register_query(query.clone())
        .await
        .expect("register gather query");
}

async fn unregister(app: &TestApp, query: &GatherQuery) {
    sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
        .bind(&query.query_id)
        .execute(&app.db)
        .await
        .expect("clean up gather query");
}

async fn create_item(app: &TestApp, title: &str, status: i16, created: i64) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("feed test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;

    sqlx::query("UPDATE item SET created = $1 WHERE id = $2")
        .bind(created)
        .bind(id)
        .execute(&app.db)
        .await
        .expect("date item");

    id
}

async fn delete_items(app: &TestApp, ids: &[Uuid]) {
    for id in ids {
        app.state.items().delete(*id, &admin()).await.ok();
    }
}

/// The feed router for `query`, wrapped in the session layer.
///
/// The handler resolves the viewer from the session, so it needs the same
/// `SessionManagerLayer` the application wraps its router in; without it every
/// request fails extraction rather than being treated as anonymous.
async fn feed_router(app: &TestApp, query: &GatherQuery) -> axum::Router {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let session_layer = trovato_kernel::session::create_session_layer(
        &redis_url,
        tower_sessions::cookie::SameSite::Strict,
    )
    .await
    .expect("create session layer");

    build_feed_router(std::slice::from_ref(query))
        .with_state(app.state.clone())
        .layer(session_layer)
}

/// Fetch a feed through a router built from `query`, the way `main` builds it at
/// startup.
///
/// The shared `TestApp` router is built once, before these queries exist, so the
/// feed router is built here from the query under test. That is the code path
/// `main` uses; what it cannot cover is the merge into the application router,
/// which is one line in `main` and in the `TestApp` fixture.
async fn fetch_feed(
    app: &TestApp,
    query: &GatherQuery,
    path: &str,
) -> (StatusCode, String, String) {
    let response = feed_router(app, query)
        .await
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .expect("serve feed");

    let status = response.status();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");

    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// A declared feed is served, as RSS, with the query's items in it.
#[test]
fn a_declared_feed_serves_rss_with_the_query_results() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-test-{stamp}.xml");
        let query = feed_query(&format!("feed_test_{stamp}"), &path);

        let base = chrono::Utc::now().timestamp();
        let newer = format!("Newer Post {stamp}");
        let older = format!("Older Post {stamp}");
        let ids = vec![
            create_item(app, &older, 1, base).await,
            create_item(app, &newer, 1, base + 60).await,
        ];

        register(app, &query).await;
        let (status, content_type, body) = fetch_feed(app, &query, &path).await;

        assert_eq!(status, StatusCode::OK, "the feed path must be served");
        assert_eq!(
            content_type, "application/rss+xml; charset=utf-8",
            "an aggregator dispatches on the content type"
        );
        assert!(
            body.starts_with("<?xml version=\"1.0\""),
            "body was:\n{body}"
        );
        assert!(
            body.contains("<title>Feed Test</title>"),
            "the channel takes the feed's title, was:\n{body}"
        );
        assert!(
            body.contains("<description>What the feed is about</description>"),
            "the channel takes the feed's description, was:\n{body}"
        );
        assert!(body.contains(&format!("<title>{newer}</title>")), "{body}");
        assert!(body.contains(&format!("<title>{older}</title>")), "{body}");

        // The query sorts newest first, and the feed must preserve that order:
        // an aggregator shows entries in document order.
        let newer_at = body.find(&newer).expect("newer entry");
        let older_at = body.find(&older).expect("older entry");
        assert!(
            newer_at < older_at,
            "the feed must keep the query's order, was:\n{body}"
        );

        delete_items(app, &ids).await;
        unregister(app, &query).await;
    });
}

/// The reason this is kernel-side rather than a plugin reading `query-items`:
/// the feed is an execution of the query as the fetching viewer, so the access
/// and status filtering the gather pipeline applies is applied here too.
#[test]
fn an_unpublished_item_is_absent_from_the_anonymous_feed() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-access-{stamp}.xml");
        let query = feed_query(&format!("feed_access_{stamp}"), &path);

        let base = chrono::Utc::now().timestamp();
        let published = format!("Published Post {stamp}");
        let unpublished = format!("Unpublished Post {stamp}");
        let ids = vec![
            create_item(app, &published, 1, base).await,
            create_item(app, &unpublished, 0, base + 60).await,
        ];

        register(app, &query).await;
        let (status, _, body) = fetch_feed(app, &query, &path).await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&published),
            "the published item belongs in the feed, was:\n{body}"
        );
        assert!(
            !body.contains(&unpublished),
            "an unpublished item must not reach an anonymous feed, was:\n{body}"
        );

        delete_items(app, &ids).await;
        unregister(app, &query).await;
    });
}

/// Entries link the item's URL alias when it has one, absolute, because a feed
/// entry is read outside the site.
#[test]
fn entry_links_are_absolute_and_use_the_url_alias() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-alias-{stamp}.xml");
        let query = feed_query(&format!("feed_alias_{stamp}"), &path);

        let title = format!("Aliased Post {stamp}");
        let id = create_item(app, &title, 1, chrono::Utc::now().timestamp()).await;

        let alias = format!("/feed-alias-{stamp}");
        UrlAlias::create(
            &app.db,
            CreateUrlAlias {
                source: format!("/item/{id}"),
                alias: alias.clone(),
                language: Some("en".to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create alias");

        register(app, &query).await;
        let (_, _, body) = fetch_feed(app, &query, &path).await;
        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();

        assert!(
            body.contains(&format!("<link>{site_url}{alias}</link>")),
            "entry links must be absolute and use the alias, was:\n{body}"
        );
        assert!(
            body.contains(&format!(
                "<guid isPermaLink=\"true\">{site_url}{alias}</guid>"
            )),
            "the guid must match the link, was:\n{body}"
        );

        sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&alias)
            .execute(&app.db)
            .await
            .expect("clean up alias");
        delete_items(app, &[id]).await;
        unregister(app, &query).await;
    });
}

/// An empty query still produces a well-formed document: an aggregator polling a
/// feed with nothing in it yet must not see a parse error.
#[test]
fn a_feed_with_no_results_is_still_valid_rss() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-empty-{stamp}.xml");
        let mut query = feed_query(&format!("feed_empty_{stamp}"), &path);
        // A filter nothing can match, so the query is genuinely empty whatever
        // else is in the shared database.
        query.definition.item_type = Some(format!("no_such_type_{stamp}"));

        register(app, &query).await;
        let (status, _, body) = fetch_feed(app, &query, &path).await;

        assert_eq!(status, StatusCode::OK, "body was: {body}");
        assert!(body.contains("<channel>"), "{body}");
        assert!(!body.contains("<item>"), "{body}");
        assert!(body.ends_with("</channel>\n</rss>\n"), "{body}");

        unregister(app, &query).await;
    });
}

/// A query with no feed declared gets no route and no autodiscovery link, so
/// adding this feature does not put a feed on every gather query.
#[test]
fn a_query_without_a_feed_is_not_served() {
    run_test(async {
        let app = shared_app().await;

        let stamp = Uuid::now_v7().simple().to_string();
        let mut query = feed_query(&format!("feed_none_{stamp}"), "/rss/unused.xml");
        query.display.feed = None;

        assert!(
            feed_links(std::slice::from_ref(&query)).is_empty(),
            "no feed declared means no autodiscovery link"
        );

        let response = feed_router(app, &query)
            .await
            .oneshot(Request::get("/rss/unused.xml").body(Body::empty()).unwrap())
            .await
            .expect("serve");

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "no route may be registered for a query with no feed"
        );
    });
}
