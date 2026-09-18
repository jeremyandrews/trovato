#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `/sitemap.xml` and `/robots.txt`, served by the real router.
//!
//! The unit tests in `routes::sitemap` cover the rendering of one entry. What is
//! pinned here is what the route actually serves, the defect being that every
//! `<loc>` carried a bare path — `/blog/hello` rather than
//! `https://example.com/blog/hello`. The sitemap protocol requires a full URL,
//! and a sitemap is fetched with no request context to resolve a relative path
//! against, so every address in it was unusable.
//!
//! The second half is translations: a translated item is a page at more than one
//! address, and before this each item appeared once, in the default language,
//! with no `hreflang` alternates at all.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateUrlAlias, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_F403_0000_0002;

/// This file's own item type, so its items cannot be confused with another
/// file's in a sitemap that necessarily lists the whole site.
const ITEM_TYPE: &str = "sitemap_test";

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
         VALUES ($1, 'Sitemap Test', 'Fixture type for sitemap tests', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed sitemap test item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .create(
            ITEM_TYPE,
            "Sitemap Test",
            Some("Fixture type for sitemap tests"),
            settings,
        )
        .await
        .ok();
}

/// A published item with a URL alias unique to this run.
async fn create_aliased_item(app: &TestApp, slug: &str) -> (Uuid, String) {
    ensure_item_type(app).await;

    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: "Sitemap fixture".to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("sitemap test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;

    let alias = format!("/{slug}");
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

    (id, alias)
}

async fn translate(app: &TestApp, item: Uuid, language: &str) {
    sqlx::query(
        "INSERT INTO item_translation (item_id, language, title, fields) \
         VALUES ($1, $2, 'Tradotto', '{}'::jsonb) \
         ON CONFLICT (item_id, language) DO NOTHING",
    )
    .bind(item)
    .bind(language)
    .execute(&app.db)
    .await
    .expect("record translation");
}

async fn fetch(app: &TestApp, path: &str) -> String {
    let response = app
        .request(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("build request"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{path} must be served");
    let body = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8(body.to_vec()).expect("utf-8 body")
}

/// The defect, at the layer it lives: the served document's `<loc>` values were
/// paths, which the sitemap protocol does not accept.
#[test]
fn every_loc_the_route_serves_is_absolute() {
    run_test(async {
        let app = shared_app().await;
        let slug = format!("sitemap-absolute-{}", Uuid::now_v7().simple());
        let (_id, alias) = create_aliased_item(app, &slug).await;

        let xml = fetch(app, "/sitemap.xml").await;
        let base = app.state.runtime().site_url.trim_end_matches('/');

        assert!(
            xml.contains(&format!("<loc>{base}{alias}</loc>")),
            "the item's absolute address is missing from the sitemap"
        );
        assert!(
            !xml.contains(&format!("<loc>{alias}</loc>")),
            "a relative <loc> is the defect this fixes"
        );
        // Not one entry anywhere in the document may be relative.
        for line in xml.lines().filter(|l| l.contains("<loc>")) {
            let loc = line
                .trim()
                .trim_start_matches("<loc>")
                .trim_end_matches("</loc>");
            assert!(
                loc.starts_with("http://") || loc.starts_with("https://"),
                "relative <loc> served: {loc}"
            );
        }
    });
}

/// A translated item is a page at more than one address. Each one is listed, and
/// each carries the whole alternate set including `x-default`.
#[test]
fn a_translated_item_is_listed_at_every_address_with_alternates() {
    run_test(async {
        let app = shared_app().await;
        let slug = format!("sitemap-translated-{}", Uuid::now_v7().simple());
        let (id, alias) = create_aliased_item(app, &slug).await;
        translate(app, id, "it").await;

        let xml = fetch(app, "/sitemap.xml").await;
        let base = app.state.runtime().site_url.trim_end_matches('/');

        assert!(
            xml.contains(&format!("<loc>{base}{alias}</loc>")),
            "the default-language address is missing"
        );
        assert!(
            xml.contains(&format!("<loc>{base}/it{alias}</loc>")),
            "the Italian address is missing: a translated page was listed once"
        );
        assert!(
            xml.contains(&format!(
                "<xhtml:link rel=\"alternate\" hreflang=\"it\" href=\"{base}/it{alias}\"/>"
            )),
            "no Italian hreflang alternate"
        );
        assert!(
            xml.contains(&format!(
                "<xhtml:link rel=\"alternate\" hreflang=\"x-default\" href=\"{base}{alias}\"/>"
            )),
            "no x-default alternate"
        );
        assert!(
            xml.contains("xmlns:xhtml=\"http://www.w3.org/1999/xhtml\""),
            "xhtml:link needs its namespace declared or the document is invalid"
        );
    });
}

/// An untranslated item is still listed once, with no alternates: a lone
/// `xhtml:link` pointing at the entry's own address says nothing.
#[test]
fn an_untranslated_item_gets_one_entry() {
    run_test(async {
        let app = shared_app().await;
        let slug = format!("sitemap-single-{}", Uuid::now_v7().simple());
        let (_id, alias) = create_aliased_item(app, &slug).await;

        let xml = fetch(app, "/sitemap.xml").await;
        let base = app.state.runtime().site_url.trim_end_matches('/');

        let entries = xml.matches(&format!("<loc>{base}{alias}</loc>")).count();
        assert_eq!(entries, 1, "an untranslated item is listed once");
        assert!(
            !xml.contains(&format!("href=\"{base}/it{alias}\"")),
            "no alternate for a language the item does not exist in"
        );
    });
}

/// `robots.txt` names the sitemap by a full URL, which the specification
/// requires for the same reason.
#[test]
fn robots_txt_points_at_an_absolute_sitemap() {
    run_test(async {
        let app = shared_app().await;
        let body = fetch(app, "/robots.txt").await;
        let base = app.state.runtime().site_url.trim_end_matches('/');

        assert!(
            body.contains(&format!("Sitemap: {base}/sitemap.xml")),
            "got {body}"
        );
        assert!(
            !body.contains("Sitemap: /sitemap.xml"),
            "a relative sitemap reference is what this fixes"
        );
    });
}
