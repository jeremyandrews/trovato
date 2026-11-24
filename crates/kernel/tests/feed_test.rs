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
    GatherQuery, QueryDefinition, QueryDisplay, QuerySort, SortDirection,
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
        .replace(
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
            items_per_page: Some(10),
            // Built from JSON, the shape a `gather_query.display` row holds.
            feed: Some(
                serde_json::from_value(serde_json::json!({
                    "path": path,
                    "title": "Feed Test",
                    "description": "What the feed is about",
                    "items": 20,
                }))
                .expect("feed config"),
            ),
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
    create_item_with(
        app,
        title,
        status,
        created,
        Uuid::nil(),
        serde_json::json!({}),
    )
    .await
}

/// An item with an author and field values.
async fn create_item_with(
    app: &TestApp,
    title: &str,
    status: i16,
    created: i64,
    author_id: Uuid,
    fields: serde_json::Value,
) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: title.to_string(),
                author_id,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
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
    let redis_url = trovato_test_utils::env::redis_url();
    let session_layer = trovato_kernel::session::create_session_layer(
        &redis_url,
        tower_sessions::cookie::SameSite::Strict,
        trovato_kernel::session::CookieSecure::Always,
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
            body.contains(&format!("<guid isPermaLink=\"false\">urn:uuid:{id}</guid>")),
            "the guid is the item uuid, not the link, was:\n{body}"
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

// ---------------------------------------------------------------------------
// B127: what an entry carries, Atom, and conditional GET.
// ---------------------------------------------------------------------------

/// Fetch `request` through the feed router for `query`, keeping the headers.
async fn send(
    app: &TestApp,
    query: &GatherQuery,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = feed_router(app, query)
        .await
        .oneshot(request)
        .await
        .expect("serve feed");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The Atom sibling of an RSS path, as the router registers it.
fn atom_of(path: &str) -> String {
    path.replace(".xml", ".atom")
}

/// A user to author items, with the given account name.
async fn create_user(app: &TestApp, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(name)
    .bind(format!("{id}@feed-test.invalid"))
    .execute(&app.db)
    .await
    .expect("create user");
    id
}

/// A category with two terms; returns the term ids.
async fn create_terms(app: &TestApp, stamp: &str, labels: [&str; 2]) -> (String, Vec<Uuid>) {
    let category = format!("ft{}", &stamp[stamp.len() - 20..]);
    sqlx::query(
        "INSERT INTO category (id, label, hierarchy, weight) VALUES ($1, 'Feed Test', 0, 0)",
    )
    .bind(&category)
    .execute(&app.db)
    .await
    .expect("create category");
    let now = chrono::Utc::now().timestamp();
    let mut ids = Vec::new();
    for (n, label) in labels.iter().enumerate() {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO category_tag (id, category_id, label, slug, weight, created, changed) \
             VALUES ($1, $2, $3, $4, 0, $5, $5)",
        )
        .bind(id)
        .bind(&category)
        .bind(*label)
        .bind(format!("t{n}-{stamp}"))
        .bind(now)
        .execute(&app.db)
        .await
        .expect("create term");
        ids.push(id);
    }
    (category, ids)
}

async fn cleanup(
    app: &TestApp,
    query: &GatherQuery,
    items: &[Uuid],
    users: &[Uuid],
    category: &str,
) {
    delete_items(app, items).await;
    for user in users {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user)
            .execute(&app.db)
            .await
            .ok();
    }
    sqlx::query("DELETE FROM category WHERE id = $1")
        .bind(category)
        .execute(&app.db)
        .await
        .ok();
    unregister(app, query).await;
}

/// The defect: an entry carried title, link, description and date only, the
/// guid was the link, and the description was the whole stored body.
#[test]
fn an_entry_carries_its_creator_categories_summary_and_a_uuid_id() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-entry-{stamp}.xml");
        let mut query = feed_query(&format!("feed_entry_{stamp}"), &path);
        isolate(&mut query, &stamp);

        let author_name = format!("author_{}", &stamp[stamp.len() - 12..]);
        let author = create_user(app, &author_name).await;
        let email_named = create_user(
            app,
            &format!("mail{}@example.com", &stamp[stamp.len() - 12..]),
        )
        .await;
        let (category, terms) = create_terms(app, &stamp, ["Rust", "Feeds & Readers"]).await;
        let body = format!("<p>Opening &amp; {}</p>", "lorem ".repeat(120));

        let base = chrono::Utc::now().timestamp();
        let tagged = create_item_with(
            app,
            &format!("Tagged {stamp}"),
            1,
            base + 60,
            author,
            serde_json::json!({
                "field_tags": terms.iter().map(Uuid::to_string).collect::<Vec<_>>(),
                "field_body": {"value": body, "format": "filtered_html"},
            }),
        )
        .await;
        let summarised = create_item_with(
            app,
            &format!("Summarised {stamp}"),
            1,
            base,
            email_named,
            serde_json::json!({
                "field_summary": {"value": "A <em>hand-written</em> teaser."},
                "field_body": {"value": "<p>Not this.</p>"},
            }),
        )
        .await;

        register(app, &query).await;
        let (status, _, rss) = fetch_feed(app, &query, &path).await;
        assert_eq!(status, StatusCode::OK, "{rss}");

        assert!(
            rss.contains(&format!("<dc:creator>{author_name}</dc:creator>")),
            "{rss}"
        );
        assert!(!rss.contains("@example.com"), "never an email: {rss}");
        assert!(rss.contains("<category>Rust</category>"), "{rss}");
        assert!(
            rss.contains("<category>Feeds &amp; Readers</category>"),
            "{rss}"
        );
        assert!(
            rss.contains(&format!(
                "<guid isPermaLink=\"false\">urn:uuid:{tagged}</guid>"
            )),
            "{rss}"
        );
        assert!(
            rss.contains("<description>Opening &amp; lorem lorem"),
            "the body, stripped and escaped once: {rss}"
        );
        assert!(!rss.contains("&lt;p&gt;"), "no markup in a summary: {rss}");
        assert!(
            rss.contains("<description>A hand-written teaser.</description>"),
            "the summary field wins over the body: {rss}"
        );
        assert!(!rss.contains("Not this."), "{rss}");
        assert!(
            !rss.contains("content:encoded>"),
            "full_content is off: {rss}"
        );

        cleanup(
            app,
            &query,
            &[tagged, summarised],
            &[author, email_named],
            &category,
        )
        .await;
    });
}

/// The defect: the guid was the link, so a new alias made every reader show
/// the post again as new.
#[test]
fn an_alias_change_leaves_the_entry_id_unchanged() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-move-{stamp}.xml");
        let mut query = feed_query(&format!("feed_move_{stamp}"), &path);
        isolate(&mut query, &stamp);
        let id = create_item(
            app,
            &format!("Moved {stamp}"),
            1,
            chrono::Utc::now().timestamp(),
        )
        .await;
        register(app, &query).await;

        let guid = |body: &str| {
            let start = body.find("<guid").expect("guid");
            body[start..body[start..].find("</guid>").unwrap() + start].to_string()
        };

        let (_, _, before) = fetch_feed(app, &query, &path).await;
        let alias = format!("/moved-{stamp}");
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
        let (_, _, after) = fetch_feed(app, &query, &path).await;

        assert!(after.contains(&format!("{alias}</link>")), "{after}");
        assert!(!before.contains(&format!("{alias}</link>")), "{before}");
        assert_eq!(
            guid(&before),
            guid(&after),
            "the id must not follow the link"
        );

        sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&alias)
            .execute(&app.db)
            .await
            .ok();
        delete_items(app, &[id]).await;
        unregister(app, &query).await;
    });
}

/// Both documents parse with a real feed parser, and the parser reads the same
/// id, author, terms and summary out of each.
#[test]
fn rss_and_atom_both_parse_with_a_feed_parser() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-parse-{stamp}.xml");
        let mut query = feed_query(&format!("feed_parse_{stamp}"), &path);
        isolate(&mut query, &stamp);
        // `full_content` set the way a display row sets it.
        query.display.feed = Some(
            serde_json::from_value(serde_json::json!({"path": path, "full_content": true}))
                .unwrap(),
        );
        let author_name = format!("parser_{}", &stamp[stamp.len() - 12..]);
        let author = create_user(app, &author_name).await;
        let (category, terms) = create_terms(app, &stamp, ["One", "Two"]).await;
        let id = create_item_with(
            app,
            &format!("Parsed & <checked> {stamp}"),
            1,
            chrono::Utc::now().timestamp(),
            author,
            serde_json::json!({
                "field_tags": terms.iter().map(Uuid::to_string).collect::<Vec<_>>(),
                "field_body": {"value": "<p>Body ]]> with a terminator.</p>", "format": "filtered_html"},
            }),
        )
        .await;
        register(app, &query).await;

        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();
        let atom_path = atom_of(&path);
        for (fetch_path, kind, content_type) in [
            (
                path.as_str(),
                feed_rs::model::FeedType::RSS2,
                "application/rss+xml; charset=utf-8",
            ),
            (
                atom_path.as_str(),
                feed_rs::model::FeedType::Atom,
                "application/atom+xml; charset=utf-8",
            ),
        ] {
            let (status, headers, body) = send(
                app,
                &query,
                Request::get(fetch_path).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{fetch_path}: {body}");
            assert_eq!(
                headers.get("content-type").unwrap().to_str().unwrap(),
                content_type
            );
            let feed = feed_rs::parser::parse(body.as_bytes())
                .unwrap_or_else(|e| panic!("{fetch_path} must parse: {e}\n{body}"));
            assert_eq!(feed.feed_type, kind);
            assert!(feed.updated.is_some() || kind == feed_rs::model::FeedType::RSS2);
            assert!(
                feed.links
                    .iter()
                    .any(|l| l.href == format!("{site_url}{fetch_path}")),
                "{fetch_path}: a rel=self link to itself: {:?}",
                feed.links
            );

            let entry = feed.entries.first().expect("one entry");
            assert_eq!(entry.id, format!("urn:uuid:{id}"), "{fetch_path}");
            assert_eq!(
                entry.title.as_ref().unwrap().content,
                format!("Parsed & <checked> {stamp}"),
                "the title is escaped exactly once"
            );
            assert_eq!(
                entry.authors.first().map(|a| a.name.as_str()),
                Some(author_name.as_str())
            );
            let mut cats: Vec<&str> = entry.categories.iter().map(|c| c.term.as_str()).collect();
            cats.sort_unstable();
            assert_eq!(cats, vec!["One", "Two"], "{fetch_path}");
            assert_eq!(
                entry.summary.as_ref().unwrap().content,
                "Body ]]> with a terminator.",
                "{fetch_path}"
            );
            let html = entry
                .content
                .as_ref()
                .and_then(|c| c.body.clone())
                .unwrap_or_default();
            assert!(
                html.contains("<p>Body ]]&gt; with a terminator.</p>")
                    || html.contains("<p>Body ]]> with a terminator.</p>"),
                "{fetch_path}: {html}"
            );
            assert!(
                entry.links.iter().any(|l| l.href.starts_with(&site_url)),
                "absolute links: {:?}",
                entry.links
            );
        }

        cleanup(app, &query, &[id], &[author], &category).await;
    });
}

/// The defect: no validators, so every aggregator poll rebuilt the document.
#[test]
fn a_repeat_request_with_the_etag_is_not_modified() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-etag-{stamp}.xml");
        let mut query = feed_query(&format!("feed_etag_{stamp}"), &path);
        isolate(&mut query, &stamp);
        let id = create_item(
            app,
            &format!("Polled {stamp}"),
            1,
            chrono::Utc::now().timestamp(),
        )
        .await;
        register(app, &query).await;

        for fetch_path in [path.clone(), atom_of(&path)] {
            let (status, headers, _) = send(
                app,
                &query,
                Request::get(&fetch_path).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let etag = headers
                .get("etag")
                .expect("an ETag")
                .to_str()
                .unwrap()
                .to_string();
            let last_modified = headers
                .get("last-modified")
                .expect("a Last-Modified")
                .to_str()
                .unwrap()
                .to_string();
            assert_eq!(
                headers.get("cache-control").unwrap().to_str().unwrap(),
                "public, max-age=300",
                "an anonymous feed is publicly cacheable, briefly"
            );

            let (status, headers, body) = send(
                app,
                &query,
                Request::get(&fetch_path)
                    .header("if-none-match", &etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{fetch_path}");
            assert!(body.is_empty());
            assert_eq!(headers.get("etag").unwrap().to_str().unwrap(), etag);

            let (status, _, _) = send(
                app,
                &query,
                Request::get(&fetch_path)
                    .header("if-modified-since", &last_modified)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{fetch_path}");
        }

        // An edit to an entry is a new document.
        let (_, headers, _) = send(
            app,
            &query,
            Request::get(&path).body(Body::empty()).unwrap(),
        )
        .await;
        let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();
        sqlx::query("UPDATE item SET changed = changed + 10 WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
        let (status, headers, _) = send(
            app,
            &query,
            Request::get(&path)
                .header("if-none-match", &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "an edited entry must not be 304");
        assert_ne!(headers.get("etag").unwrap().to_str().unwrap(), etag);

        delete_items(app, &[id]).await;
        unregister(app, &query).await;
    });
}

/// Atom is the same execution as RSS: an unpublished item is in neither, and a
/// gather the viewer may not run is refused in both.
#[test]
fn an_anonymous_viewer_sees_no_unpublished_or_denied_item_in_either_format() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-deny-{stamp}.xml");
        let query = feed_query(&format!("feed_deny_{stamp}"), &path);
        let base = chrono::Utc::now().timestamp();
        let published = format!("Visible {stamp}");
        let unpublished = format!("Hidden {stamp}");
        let ids = vec![
            create_item(app, &published, 1, base).await,
            create_item(app, &unpublished, 0, base + 60).await,
        ];
        register(app, &query).await;

        for fetch_path in [path.clone(), atom_of(&path)] {
            let (status, _, body) = send(
                app,
                &query,
                Request::get(&fetch_path).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains(&published), "{fetch_path}: {body}");
            assert!(!body.contains(&unpublished), "{fetch_path}: {body}");
        }

        let gated_path = format!("/rss/feed-gated-{stamp}.xml");
        let mut gated = feed_query(&format!("feed_gated_{stamp}"), &gated_path);
        gated.definition.access = Some(
            serde_json::from_value(serde_json::json!({"permission": "administer site"})).unwrap(),
        );
        register(app, &gated).await;
        for fetch_path in [gated_path.clone(), atom_of(&gated_path)] {
            let (status, _, body) = send(
                app,
                &gated,
                Request::get(&fetch_path).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{fetch_path}");
            assert!(!body.contains(&published), "{fetch_path}: {body}");
        }

        delete_items(app, &ids).await;
        unregister(app, &query).await;
        unregister(app, &gated).await;
    });
}

/// Narrow `query` to items whose title carries `stamp`, so items other tests in
/// this file create concurrently under the same type cannot change its result
/// (and with it the ETag) between two requests.
fn isolate(query: &mut GatherQuery, stamp: &str) {
    query.definition.filters.push(
        serde_json::from_value(serde_json::json!({
            "field": "title",
            "operator": "contains",
            // Not the whole 32-digit stamp: that deserializes as a uuid.
            "value": &stamp[8..],
        }))
        .expect("title filter"),
    );
}

// ---------------------------------------------------------------------------
// B127 part two: feeds with a path argument.
// ---------------------------------------------------------------------------

/// The shipped `blog_listing` gather, as `trovato_blog`'s migrations leave it,
/// registered with the gather service.
async fn shipped_blog_listing(app: &TestApp) -> GatherQuery {
    common::ensure_plugin_tables(app, "trovato_blog");
    let (definition, display, label, description, changed): (
        serde_json::Value,
        serde_json::Value,
        String,
        Option<String>,
        i64,
    ) = sqlx::query_as(
        "SELECT definition, display, label, description, changed \
         FROM gather_query WHERE query_id = 'blog_listing'",
    )
    .fetch_one(&app.db)
    .await
    .expect("blog_listing is seeded");
    let query: GatherQuery = serde_json::from_value(serde_json::json!({
        "query_id": "blog_listing",
        "label": label,
        "description": description,
        "definition": definition,
        "display": display,
        "plugin": "blog",
        "created": 0,
        "changed": changed,
    }))
    .expect("blog_listing deserializes");
    register(app, &query).await;
    query
}

/// The `blog` type, if no earlier test registered it, with the fields
/// `trovato_blog` declares.
async fn ensure_blog_type(app: &TestApp) {
    if app.state.content_types().exists("blog") {
        return;
    }
    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ('blog', 'Blog Post', 'A blog entry', true, 'Title', 'blog', $1) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(&settings)
    .execute(&app.db)
    .await
    .expect("seed blog type");
    app.state
        .content_types()
        .replace("blog", "Blog Post", Some("A blog entry"), settings)
        .await
        .ok();
}

/// A term in the kernel's `tags` vocabulary, which the blog's tag route binds.
async fn create_tag(app: &TestApp, label: &str, slug: &str) -> Uuid {
    let id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO category_tag (id, category_id, label, slug, weight, created, changed) \
         VALUES ($1, 'tags', $2, $3, 0, $4, $4)",
    )
    .bind(id)
    .bind(label)
    .bind(slug)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("create tag");
    id
}

async fn create_blog_post(
    app: &TestApp,
    title: &str,
    status: i16,
    author: Uuid,
    tag: Uuid,
) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "blog".to_string(),
                title: title.to_string(),
                author_id: author,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({
                    "field_body": {"value": format!("<p>{title} body.</p>"), "format": "filtered_html"},
                    "field_tags": [tag.to_string()],
                })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("feed test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create blog post")
        .id
}

async fn get(app: &TestApp, query: &GatherQuery, path: &str) -> (StatusCode, String) {
    let (status, _, body) = send(app, query, Request::get(path).body(Body::empty()).unwrap()).await;
    (status, body)
}

/// One feed declaration on the stock blog gather gives a feed per author and a
/// feed per tag, in both formats, each bound through the gather's own route
/// mapping; an unknown author or tag is a 404, and an unpublished post is in
/// none of them.
#[test]
fn the_blog_gather_serves_a_feed_per_author_and_per_tag() {
    run_test(async {
        let app = shared_app().await;
        ensure_blog_type(app).await;
        let blog = shipped_blog_listing(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let s = &stamp[stamp.len() - 12..];
        let alice_name = format!("alice{s}");
        let alice = create_user(app, &alice_name).await;
        let bob = create_user(app, &format!("bob{s}")).await;
        let rust_slug = format!("rust-{s}");
        let rust = create_tag(app, &format!("Rust {s}"), &rust_slug).await;
        let go = create_tag(app, &format!("Go {s}"), &format!("go-{s}")).await;

        let by_alice = format!("Alice on Rust {s}");
        let by_bob = format!("Bob on Go {s}");
        let draft = format!("Alice draft {s}");
        let ids = vec![
            create_blog_post(app, &by_alice, 1, alice, rust).await,
            create_blog_post(app, &by_bob, 1, bob, go).await,
            create_blog_post(app, &draft, 0, alice, rust).await,
        ];

        for ext in ["xml", "atom"] {
            let (status, body) =
                get(app, &blog, &format!("/blog/author/{alice_name}/feed.{ext}")).await;
            assert_eq!(status, StatusCode::OK, "author feed .{ext}: {body}");
            assert!(body.contains(&by_alice), "author feed .{ext}: {body}");
            assert!(!body.contains(&by_bob), "only alice's posts: {body}");
            assert!(!body.contains(&draft), "no unpublished post: {body}");
            assert!(
                body.contains(&format!("/blog/author/{alice_name}/feed.{ext}")),
                "the self link carries the argument: {body}"
            );
            feed_rs::parser::parse(body.as_bytes()).expect("parses");

            let (status, body) =
                get(app, &blog, &format!("/blog/tag/{rust_slug}/feed.{ext}")).await;
            assert_eq!(status, StatusCode::OK, "tag feed .{ext}: {body}");
            assert!(body.contains(&by_alice), "tag feed .{ext}: {body}");
            assert!(!body.contains(&by_bob), "only rust posts: {body}");
            assert!(!body.contains(&draft), "no unpublished post: {body}");

            let (status, body) =
                get(app, &blog, &format!("/blog/author/nobody{s}/feed.{ext}")).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "unknown author .{ext}: {body}"
            );
            let (status, body) =
                get(app, &blog, &format!("/blog/tag/nothing-{s}/feed.{ext}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "unknown tag .{ext}: {body}");
        }

        let (status, _) = get(app, &blog, "/blog/feed.xml").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the site-wide blog feed is still served"
        );

        delete_items(app, &ids).await;
        sqlx::query("DELETE FROM category_tag WHERE id = ANY($1)")
            .bind(vec![rust, go])
            .execute(&app.db)
            .await
            .ok();
        for user in [alice, bob] {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(user)
                .execute(&app.db)
                .await
                .ok();
        }
    });
}

/// The page that shows the gather with an argument advertises the feed with
/// the same argument.
#[test]
fn the_author_page_advertises_the_author_feed() {
    run_test(async {
        let app = shared_app().await;
        ensure_blog_type(app).await;
        let blog = shipped_blog_listing(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let name = format!("carol{}", &stamp[stamp.len() - 12..]);
        let carol = create_user(app, &name).await;

        let redis_url = trovato_test_utils::env::redis_url();
        let session_layer = trovato_kernel::session::create_session_layer(
            &redis_url,
            tower_sessions::cookie::SameSite::Strict,
            trovato_kernel::session::CookieSecure::Always,
        )
        .await
        .expect("create session layer");
        let router = trovato_kernel::routes::gather_routes::build_gather_route_router(
            std::slice::from_ref(&blog),
        )
        .with_state(app.state.clone())
        .layer(axum::Extension(
            trovato_kernel::middleware::language::ResolvedLanguage("en".to_string()),
        ))
        .layer(session_layer);

        let response = router
            .oneshot(
                Request::get(format!("/blog/author/{name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("serve page");
        assert_eq!(response.status(), StatusCode::OK);
        let html = String::from_utf8_lossy(
            &to_bytes(response.into_body(), 4 * 1024 * 1024).await.unwrap(),
        )
        .into_owned()
        // Tera escapes `/` in an attribute; the browser reads it back as `/`.
        .replace("&#x2F;", "/");

        assert!(
            html.contains(&format!(
                "type=\"application/rss+xml\" title=\"Blog: {name}\" href=\"/blog/author/{name}/feed.xml\""
            )),
            "{html}"
        );
        assert!(
            html.contains(&format!("href=\"/blog/author/{name}/feed.atom\"")),
            "{html}"
        );

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(carol)
            .execute(&app.db)
            .await
            .ok();
    });
}

/// A passed-through argument is not looked up, so a value that matches
/// nothing the viewer can see is a 404, not an empty feed.
#[test]
fn a_pass_through_argument_matching_nothing_is_not_found() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let word = &stamp[8..];
        let path = format!("/rss/feed-word-{stamp}/{{word}}/feed.xml");
        let mut query = feed_query(&format!("feed_word_{stamp}"), &path);
        query.definition.filters.push(
            serde_json::from_value(serde_json::json!({
                "field": "title", "operator": "contains", "value": {"url_arg": "word"},
            }))
            .unwrap(),
        );
        query.display.routes = serde_json::from_value(serde_json::json!([
            {"path": format!("/words-{stamp}/{{word}}"), "params": [{"segment": "word", "param": "word"}]},
        ]))
        .unwrap();
        let title = format!("Word {word}");
        let id = create_item(app, &title, 1, chrono::Utc::now().timestamp()).await;
        register(app, &query).await;

        let (status, body) = get(
            app,
            &query,
            &format!("/rss/feed-word-{stamp}/{word}/feed.xml"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(&title), "{body}");

        let (status, _) = get(
            app,
            &query,
            &format!("/rss/feed-word-{stamp}/zz{word}/feed.atom"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        delete_items(app, &[id]).await;
        unregister(app, &query).await;
    });
}

/// The defect: the feed executed page one of the gather, so `items` above the
/// gather's page size was ignored (`items: 20` on a ten-a-page blog gave ten).
#[test]
fn a_feed_carries_its_own_item_count_not_the_page_size() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-count-{stamp}.xml");
        let mut query = feed_query(&format!("feed_count_{stamp}"), &path);
        isolate(&mut query, &stamp);
        query.display.items_per_page = Some(2);
        query.display.feed =
            Some(serde_json::from_value(serde_json::json!({"path": path, "items": 3})).unwrap());
        let base = chrono::Utc::now().timestamp();
        let mut ids = Vec::new();
        for n in 0..4 {
            ids.push(create_item(app, &format!("Counted {n} {stamp}"), 1, base + n).await);
        }
        register(app, &query).await;

        let (status, body) = get(app, &query, &path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.matches("<item>").count(), 3, "{body}");

        delete_items(app, &ids).await;
        unregister(app, &query).await;
    });
}

// ---------------------------------------------------------------------------
// B171: the channel link is the address a visitor actually uses, so a gather
// reachable only through a URL alias (no `canonical_url`, the way the stock
// blog ships) still links that alias rather than `/gather/{query_id}`.
// ---------------------------------------------------------------------------

/// Step 2's evidence: a `canonical_url` that names the same address a URL
/// alias already rewrites to a gather's `/gather/{id}` does not loop.
///
/// `check_redirect` (`crate::middleware::redirect`) is driven by
/// `RedirectCache::find`, and `path_alias_fallback`
/// (`crate::middleware::path_alias`) is driven by
/// `UrlAlias::find_by_alias_with_context`: this exercises the exact pair of
/// lookups each middleware runs, straight against the database rather than
/// through a mock. A request for `/gather/{id}` finds the redirect row and is
/// sent to the alias (one 301 hop, asserted below); a request for the alias
/// finds no redirect row at all, so `check_redirect` never fires for it a
/// second time, and the alias rewrite alone finishes the request.
///
/// That the alias rewrite's own internal re-dispatch cannot invoke
/// `check_redirect` again is a fact of how both `main.rs` and the `TestApp`
/// harness are wired, not something this test can exercise directly:
/// `path_alias_fallback`'s inner router (`shared_router` in both) is cloned
/// from `inner_router` *before* `.layer(check_redirect)` is added to the outer
/// one, so the fallback's re-dispatch runs none of the outer middleware,
/// `check_redirect` included. A real loop needs two redirect rows pointing at
/// each other; nothing here, or `sync_canonical_redirect`, writes that.
#[test]
fn a_canonical_url_duplicating_an_alias_does_not_loop() {
    run_test(async {
        let app = shared_app().await;
        common::ensure_plugin_tables(app, "trovato_redirects");

        let stamp = Uuid::now_v7().simple().to_string();
        let gather_path = format!("/gather/loop_{stamp}");
        let alias_path = format!("/loop-{stamp}");

        // What `sync_canonical_redirect` would write for `canonical_url: alias_path`.
        trovato_kernel::services::redirect::Redirect::create(
            &app.db,
            &gather_path,
            &alias_path,
            301,
            "en",
        )
        .await
        .expect("create redirect");

        // The alias already seeded for the gather, the one `canonical_url`
        // would duplicate.
        UrlAlias::create(
            &app.db,
            CreateUrlAlias {
                source: gather_path.clone(),
                alias: alias_path.clone(),
                language: Some("en".to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create alias");

        let cache = trovato_kernel::services::redirect::RedirectCache::new();

        let redirect = cache
            .find(&app.db, &gather_path, "en")
            .await
            .expect("lookup")
            .expect("the raw gather path redirects once");
        assert_eq!(redirect.destination, alias_path, "redirects to the alias");

        let second_hop = cache
            .find(&app.db, &alias_path, "en")
            .await
            .expect("lookup");
        assert!(
            second_hop.is_none(),
            "the alias itself must carry no redirect, or the hop would repeat: {second_hop:?}"
        );

        let resolved =
            UrlAlias::find_by_alias_with_context(&app.db, &alias_path, LIVE_STAGE_ID, "en")
                .await
                .expect("lookup")
                .expect("the alias resolves");
        assert_eq!(
            resolved.source, gather_path,
            "the alias rewrites back to the gather path, internally, with no further redirect"
        );

        sqlx::query("DELETE FROM redirect WHERE source = $1")
            .bind(&gather_path)
            .execute(&app.db)
            .await
            .ok();
        sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&alias_path)
            .execute(&app.db)
            .await
            .ok();
    });
}

/// The stock blog has no `canonical_url`, only a URL alias (`/blog` to
/// `/gather/blog_listing`): the defect being that the channel link was
/// `/gather/blog_listing`, the address nobody visiting the site ever uses.
#[test]
fn the_blog_gather_links_its_url_alias_not_the_raw_gather_path() {
    run_test(async {
        let app = shared_app().await;
        ensure_blog_type(app).await;
        let blog = shipped_blog_listing(app).await;

        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();

        for path in ["/blog/feed.xml", "/blog/feed.atom"] {
            let (status, body) = get(app, &blog, path).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            // The channel's own link, exactly: `{site_url}/blog` alone is a
            // prefix of the feed's self link (`/blog/feed.xml`), so it would
            // be found whatever the channel link said.
            let channel_link = if path.ends_with(".atom") {
                format!(r#"<link rel="alternate" type="text/html" href="{site_url}/blog"/>"#)
            } else {
                format!("<link>{site_url}/blog</link>")
            };
            assert!(
                body.contains(&channel_link),
                "{path}: the channel link must be the alias, was:\n{body}"
            );
            assert!(
                !body.contains("/gather/blog_listing"),
                "{path}: the raw gather path must not leak into the channel, was:\n{body}"
            );
        }
    });
}

/// `canonical_url` still wins over an alias, when a query has both: the
/// address the query itself declares as canonical is not second-guessed.
#[test]
fn a_canonical_url_still_wins_over_an_alias() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-canon-{stamp}.xml");
        let mut query = feed_query(&format!("feed_canon_{stamp}"), &path);
        query.display.canonical_url = Some(format!("/canon-{stamp}"));
        register(app, &query).await;

        // An alias for the gather's default path, which must lose to
        // `canonical_url`.
        let gather_path = format!("/gather/feed_canon_{stamp}");
        let alias_path = format!("/alias-{stamp}");
        UrlAlias::create(
            &app.db,
            CreateUrlAlias {
                source: gather_path,
                alias: alias_path.clone(),
                language: Some("en".to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create alias");

        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();
        let (status, body) = get(app, &query, &path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(&format!(
                "<link>{site_url}{}",
                query.display.canonical_url.clone().unwrap()
            )),
            "canonical_url must win over the alias: {body}"
        );
        assert!(!body.contains(&alias_path), "{body}");

        sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&alias_path)
            .execute(&app.db)
            .await
            .ok();
        unregister(app, &query).await;
    });
}

/// With neither an alias nor a `canonical_url`, the channel link is still
/// `/gather/{query_id}`: this feature must not change a gather with no address
/// of its own.
#[test]
fn with_no_alias_and_no_canonical_the_channel_link_is_the_gather_path() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-bare-{stamp}.xml");
        let query = feed_query(&format!("feed_bare_{stamp}"), &path);
        register(app, &query).await;

        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();
        let (status, body) = get(app, &query, &path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(&format!("<link>{site_url}/gather/feed_bare_{stamp}</link>")),
            "{body}"
        );

        unregister(app, &query).await;
    });
}

/// Per author and per tag feeds keep their argument home paths: this feature
/// only changes the tier below `canonical_url`, which an argument route never
/// reaches.
#[test]
fn an_argument_feeds_home_path_is_unaffected_by_the_alias() {
    run_test(async {
        let app = shared_app().await;
        ensure_blog_type(app).await;
        let blog = shipped_blog_listing(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let name = format!("dana{}", &stamp[stamp.len() - 12..]);
        let dana = create_user(app, &name).await;

        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();
        let (status, body) = get(app, &blog, &format!("/blog/author/{name}/feed.xml")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(&format!("<link>{site_url}/blog/author/{name}</link>")),
            "an argument feed's channel link is its own route, not the alias: {body}"
        );

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(dana)
            .execute(&app.db)
            .await
            .ok();
    });
}

/// An alias written after the gather is registered still reaches the feed: the
/// channel link is resolved per request (cached), not baked in at startup like
/// `canonical_url` and `display.routes` are. The ETag must change with it,
/// since the channel link is part of the document.
#[test]
fn an_alias_written_after_startup_reaches_the_feed_and_changes_the_etag() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let stamp = Uuid::now_v7().simple().to_string();
        let path = format!("/rss/feed-late-alias-{stamp}.xml");
        let query = feed_query(&format!("feed_late_{stamp}"), &path);
        register(app, &query).await;

        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();
        let gather_path = format!("/gather/feed_late_{stamp}");
        let (status, headers, before) = send(
            app,
            &query,
            Request::get(&path).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{before}");
        assert!(
            before.contains(&format!("<link>{site_url}{gather_path}</link>")),
            "no alias yet: {before}"
        );
        let etag_before = headers.get("etag").unwrap().to_str().unwrap().to_string();

        let alias_path = format!("/late-alias-{stamp}");
        UrlAlias::create(
            &app.db,
            CreateUrlAlias {
                source: gather_path.clone(),
                alias: alias_path.clone(),
                language: Some("en".to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create alias");

        // `UrlAlias::create` bumps `CacheKind::Menus`, which every server
        // (including this one) picks up on its 250ms poll
        // (`cache::generation::spawn_watcher`), the same propagation delay
        // `cache_test.rs` already waits out for other kinds. The poll bounds
        // nothing on a loaded runner, so ask until the alias shows or a
        // generous deadline passes, rather than once after a fixed sleep.
        let alias_link = format!("<link>{site_url}{alias_path}</link>");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (status, headers, after) = loop {
            let (status, headers, body) = send(
                app,
                &query,
                Request::get(&path).body(Body::empty()).unwrap(),
            )
            .await;
            if body.contains(&alias_link) || std::time::Instant::now() >= deadline {
                break (status, headers, body);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert_eq!(status, StatusCode::OK, "{after}");
        assert!(
            after.contains(&alias_link),
            "the alias must reach the feed without a restart: {after}"
        );
        let etag_after = headers.get("etag").unwrap().to_str().unwrap().to_string();
        assert_ne!(
            etag_before, etag_after,
            "the channel link changed, so the etag must too"
        );

        // The old etag must not satisfy a conditional request against the new
        // document: a real reader with the stale etag must see the new link.
        let (status, _, body) = send(
            app,
            &query,
            Request::get(&path)
                .header("if-none-match", &etag_before)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&alias_path)
            .execute(&app.db)
            .await
            .ok();
        unregister(app, &query).await;
    });
}
