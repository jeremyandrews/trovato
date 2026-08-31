#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Front page routing.
//!
//! Two behaviours are pinned here. First, `site_front_page` may name any path
//! the site serves, not only `/item/{uuid}`: a path the front handler has never
//! heard of redirects to whichever handler owns it, an item path still renders
//! inline at `/`, and a path pointing off-site is refused. Second, the default
//! promoted listing asks for promoted items rather than filtering a page of
//! published ones, so a promoted item behind a page of newer published items is
//! still listed.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// `site_front_page` is a single row of site-wide configuration and every test
/// in this file reads `/`, so they would otherwise serve each other's front
/// page. One lock covers the whole file, including the promoted-listing test:
/// it too depends on no front page being configured.
static FRONT_PAGE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Advisory-lock key guarding this file's item-type seeding, for the same
/// reason `common`'s conference seeder takes one: check-then-insert against a
/// database shared by every test process.
const TYPE_SEED_LOCK: i64 = 0x_F401_0000_0001;

/// The item type these tests create content with.
///
/// Its own type, not `conference`: these tests date items into the future to
/// control listing order, and that has no business showing up in another
/// file's conference listings.
const ITEM_TYPE: &str = "front_page_test";

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
}

/// Seed the test item type. Idempotent, and safe against concurrent seeders.
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
         VALUES ($1, 'Front Page Test', 'Fixture type for front page tests', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed front page test item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .create(
            ITEM_TYPE,
            "Front Page Test",
            Some("Fixture type for front page tests"),
            settings,
        )
        .await
        .ok();
}

/// Create a published item, optionally promoted, with an explicit `created`
/// timestamp so that listing order is under the test's control.
async fn create_item(app: &TestApp, title: &str, promote: i16, created: i64) -> Uuid {
    let id = app
        .state
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
                log: Some("front page test".to_string()),
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

async fn set_front_page(app: &TestApp, path: &str) {
    trovato_kernel::models::SiteConfig::set_front_page(&app.db, path)
        .await
        .expect("set front page");
}

async fn clear_front_page(app: &TestApp) {
    sqlx::query("DELETE FROM site_config WHERE key = 'site_front_page'")
        .execute(&app.db)
        .await
        .expect("clear front page");
}

async fn get_front_page(app: &TestApp) -> Response {
    app.request(Request::get("/").body(Body::empty()).unwrap())
        .await
}

fn location(response: &Response) -> Option<String> {
    response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A front page pointing at a route the front handler knows nothing about
/// redirects there, and the target really is served by the site.
///
/// `/search` stands in for any non-item route — a gather alias, a plugin route,
/// an aliased path. What matters is that nothing in the front handler names it.
#[test]
fn configured_non_item_route_redirects_to_that_route() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;

        set_front_page(app, "/search").await;

        let response = get_front_page(app).await;
        assert_eq!(
            response.status(),
            StatusCode::TEMPORARY_REDIRECT,
            "a non-item front page must redirect to its route"
        );
        assert_eq!(location(&response).as_deref(), Some("/search"));

        // The redirect lands somewhere the site actually serves — the point of
        // redirecting rather than teaching the front handler to render it.
        let target = app
            .request(Request::get("/search").body(Body::empty()).unwrap())
            .await;
        assert_eq!(target.status(), StatusCode::OK);

        clear_front_page(app).await;
    });
}

/// A gather-style alias path redirects the same way, with no route of that
/// shape registered anywhere in the kernel. The front handler does not look.
#[test]
fn configured_gather_alias_redirects_without_being_known() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;

        set_front_page(app, "/devices/online").await;

        let response = get_front_page(app).await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(location(&response).as_deref(), Some("/devices/online"));

        clear_front_page(app).await;
    });
}

/// A query string on `/` survives the redirect, so a paged or filtered front
/// page keeps its parameters.
#[test]
fn redirect_carries_the_query_string() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;

        set_front_page(app, "/devices/online").await;

        let response = app
            .request(Request::get("/?page=2").body(Body::empty()).unwrap())
            .await;

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            location(&response).as_deref(),
            Some("/devices/online?page=2")
        );

        clear_front_page(app).await;
    });
}

/// The behaviour that already existed: an item front page renders at `/`,
/// address bar unchanged.
#[test]
fn configured_item_still_renders_inline() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;
        ensure_item_type(app).await;

        let title = format!("Inline Front Page {}", Uuid::now_v7().simple());
        let id = create_item(app, &title, 0, chrono::Utc::now().timestamp()).await;

        set_front_page(app, &format!("/item/{id}")).await;

        let response = get_front_page(app).await;
        assert_eq!(response.status(), StatusCode::OK, "item renders at /");
        assert_eq!(
            location(&response),
            None,
            "an item front page must not redirect"
        );

        let body = body_text(response).await;
        assert!(
            body.contains(&title),
            "the configured item must be rendered at /, body was:\n{body}"
        );

        clear_front_page(app).await;
        delete_items(app, &[id]).await;
    });
}

/// A translation of the front-page item is content, not decoration.
///
/// The front handler rendered the configured item without ever applying the
/// translation overlay, so a translation aimed at the front page was
/// configuration nothing read: `/it` served the default-language text. The
/// language reaches the page too — same context-ordering contract as everywhere
/// else.
#[test]
fn a_front_page_translation_is_served_in_that_language() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let title = format!("Front Page {marker}");
        let id = create_item(app, &title, 0, chrono::Utc::now().timestamp()).await;

        let translated = format!("Prima Pagina {marker}");
        sqlx::query(
            "INSERT INTO item_translation (item_id, language, title, fields) \
             VALUES ($1, 'it', $2, '{}'::jsonb) \
             ON CONFLICT (item_id, language) DO UPDATE SET title = EXCLUDED.title",
        )
        .bind(id)
        .bind(&translated)
        .execute(&app.db)
        .await
        .expect("record front page translation");

        set_front_page(app, &format!("/item/{id}")).await;

        // Through `Accept-Language`: a `/it/` prefix is stripped by the alias
        // fallback, which never sees `/`. The header is the negotiator that
        // reaches this route.
        let italian = app
            .request(
                Request::get("/")
                    .header("Accept-Language", "it")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            italian.status(),
            StatusCode::OK,
            "the front page renders in Italian"
        );
        let body = body_text(italian).await;
        assert!(
            body.contains(&translated),
            "the Italian front page must show the translated title, body was:\n{body}"
        );
        assert!(
            body.contains(r#"lang="it""#),
            "the Italian front page must declare lang=\"it\""
        );

        // The default language is untouched: the overlay is applied only when
        // the request asked for another language.
        let english = get_front_page(app).await;
        let body = body_text(english).await;
        assert!(
            body.contains(&title),
            "the default front page must still show the default title"
        );
        assert!(
            !body.contains(&translated),
            "the default front page must not show the translation"
        );

        clear_front_page(app).await;
        delete_items(app, &[id]).await;
    });
}

/// A front page aimed off-site is refused, and `/` falls back to its default.
#[test]
fn external_front_page_is_rejected() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;

        for external in [
            "https://example.com/",
            "http://example.com/blog",
            // Protocol-relative, and its backslash spelling: a browser reads
            // both as another host.
            "//example.com/blog",
            "/\\example.com",
            "example.com/blog",
        ] {
            set_front_page(app, external).await;

            let response = get_front_page(app).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{external} must not be served as the front page"
            );
            assert_eq!(
                location(&response),
                None,
                "{external} must not become a redirect off-site"
            );
        }

        clear_front_page(app).await;
    });
}

/// `/` as its own front page falls back instead of redirecting to itself.
#[test]
fn front_page_set_to_root_does_not_loop() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;

        set_front_page(app, "/").await;

        let response = get_front_page(app).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(location(&response), None, "/ must not redirect to itself");

        clear_front_page(app).await;
    });
}

/// A promoted item stays on the front page however many newer published items
/// are in front of it.
///
/// Before the fix the handler asked for the ten most recent published items and
/// filtered those for promotion, so the twelve newer items below hid this one
/// completely.
#[test]
fn promoted_item_behind_newer_published_items_is_listed() {
    run_test(async {
        let app = shared_app().await;
        let _guard = FRONT_PAGE.lock().await;
        ensure_item_type(app).await;
        clear_front_page(app).await;

        // Dated ahead of everything else in the shared database so that the
        // ordering this test depends on is the ordering it gets.
        let base = chrono::Utc::now().timestamp() + 86_400;

        let title = format!("Promoted Behind The Page {}", Uuid::now_v7().simple());
        let promoted = create_item(app, &title, 1, base).await;

        let mut ids = vec![promoted];
        for i in 1..=12 {
            let filler = format!("Newer Published {i} {}", Uuid::now_v7().simple());
            ids.push(create_item(app, &filler, 0, base + i).await);
        }

        // The precondition, pinned: the promoted item is not in the first page
        // of published items, which is all the old handler ever looked at.
        let first_page = app
            .state
            .items()
            .list_published(10, 0)
            .await
            .expect("list published");
        assert!(
            !first_page.iter().any(|i| i.id == promoted),
            "test setup: the promoted item must be behind the first page of published items"
        );

        let body = body_text(get_front_page(app).await).await;
        assert!(
            body.contains(&format!("/item/{promoted}")),
            "a promoted item must be listed however many newer published items precede it"
        );

        delete_items(app, &ids).await;
    });
}
