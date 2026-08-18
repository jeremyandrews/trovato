#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Head metadata on a rendered item page.
//!
//! The unit tests in `content::page_meta` cover what the metadata is derived
//! from. What is pinned here is that it reaches `<head>` of the served page at
//! all, which is the defect: a plugin implementing `tap_item_view` can only
//! append to the body, so before this the site emitted JSON-LD and no meta
//! description, no canonical link, and no Open Graph tags.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateUrlAlias, UrlAlias};
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding, for the reason the
/// conference seeder takes one: check-then-insert against a shared database.
const TYPE_SEED_LOCK: i64 = 0x_F402_0000_0001;

/// This file's own item type. `page_meta_test` rather than a shared fixture
/// type because these tests need one Blocks field and one long-text field, and
/// they assert on what the head says about the type.
const ITEM_TYPE: &str = "page_meta_test";

/// An article-typed variant, so the `og:type` and `article:*` behaviour is
/// exercised against a real render rather than only in the unit tests.
const ARTICLE_TYPE: &str = "blog";

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
}

/// Seed an item type with a body field and a blocks field. Idempotent, and safe
/// against concurrent seeders.
async fn ensure_item_type(app: &TestApp, machine_name: &str, label: &str) {
    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    let fields = vec![
        FieldDefinition::new("field_body", FieldType::TextLong).label("Body"),
        FieldDefinition::new("field_content", FieldType::Blocks).label("Content"),
    ];
    let settings = serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() });

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $2, 'Fixture type for head metadata tests', true, 'Title', 'core', $3) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(machine_name)
    .bind(label)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed head metadata test item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .create(
            machine_name,
            label,
            Some("Fixture type for head metadata tests"),
            settings,
        )
        .await
        .ok();
}

async fn create_item(
    app: &TestApp,
    item_type: &str,
    title: &str,
    fields: serde_json::Value,
) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("head metadata test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

async fn delete_item(app: &TestApp, id: Uuid) {
    app.state.items().delete(id, &admin()).await.ok();
}

/// Fetch an item page and return its `<head>`.
///
/// Asserting against the head alone keeps these tests from passing on markup
/// the SEO plugin injects into the body, which is exactly what the defect was.
async fn head_of(app: &TestApp, path: &str) -> String {
    let response = app
        .request(Request::get(path).body(Body::empty()).unwrap())
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");

    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    let body = String::from_utf8_lossy(&bytes).into_owned();

    let start = body.find("<head>").expect("a head element");
    let end = body.find("</head>").expect("a closed head element");
    assert!(end > start, "malformed document");
    body[start..end].to_string()
}

/// The whole point: description, canonical and the Open Graph set are in the
/// head of a served item page.
#[test]
fn item_page_head_carries_description_canonical_and_open_graph() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app, ITEM_TYPE, "Page Meta Test").await;

        let title = format!("Head Metadata {}", Uuid::now_v7().simple());
        let id = create_item(
            app,
            ITEM_TYPE,
            &title,
            serde_json::json!({
                "field_body": {"value": "<p>A page about metadata in the document head.</p>"},
                "field_content": [
                    {"type": "image", "data": {"file": {"url": "/files/lead.png"}}},
                ],
            }),
        )
        .await;

        let head = head_of(app, &format!("/item/{id}")).await;
        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();

        assert!(
            head.contains(
                r#"<meta name="description" content="A page about metadata in the document head.">"#
            ),
            "head must carry a meta description, was:\n{head}"
        );
        assert!(
            head.contains(&format!(
                r#"<link rel="canonical" href="{site_url}/item/{id}">"#
            )),
            "head must carry an absolute canonical link, was:\n{head}"
        );
        assert!(
            head.contains(&format!(r#"<meta property="og:title" content="{title}">"#)),
            "head must carry og:title, was:\n{head}"
        );
        assert!(
            head.contains(r#"<meta property="og:type" content="website">"#),
            "head must carry og:type, was:\n{head}"
        );
        assert!(
            head.contains(&format!(
                r#"<meta property="og:url" content="{site_url}/item/{id}">"#
            )),
            "og:url must be absolute, was:\n{head}"
        );
        assert!(
            head.contains(r#"<meta property="og:description""#),
            "head must carry og:description, was:\n{head}"
        );
        assert!(
            head.contains(&format!(
                r#"<meta property="og:image" content="{site_url}/files/lead.png">"#
            )),
            "og:image must come from the first image block, absolute, was:\n{head}"
        );
        assert!(
            head.contains(r#"<meta name="twitter:card" content="summary_large_image">"#),
            "an item with an image gets the large card, was:\n{head}"
        );

        delete_item(app, id).await;
    });
}

/// The canonical URL is the address the site links to, so an aliased item
/// canonicalizes to its alias rather than to `/item/{uuid}`.
#[test]
fn canonical_points_at_the_url_alias_when_the_item_has_one() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app, ITEM_TYPE, "Page Meta Test").await;

        let title = format!("Aliased Metadata {}", Uuid::now_v7().simple());
        let id = create_item(
            app,
            ITEM_TYPE,
            &title,
            serde_json::json!({"field_body": {"value": "Aliased."}}),
        )
        .await;

        let alias = format!("/head-metadata-{}", Uuid::now_v7().simple());
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

        let site_url = app
            .state
            .runtime()
            .site_url
            .trim_end_matches('/')
            .to_string();
        let expected = format!(r#"<link rel="canonical" href="{site_url}{alias}">"#);

        // Both addresses serve the same page, and both name the alias as
        // canonical — otherwise the two URLs compete for the same content.
        for path in [format!("/item/{id}"), alias.clone()] {
            let head = head_of(app, &path).await;
            assert!(
                head.contains(&expected),
                "GET {path} must canonicalize to the alias, was:\n{head}"
            );
        }

        sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&alias)
            .execute(&app.db)
            .await
            .expect("clean up alias");
        delete_item(app, id).await;
    });
}

/// An item with nothing to describe emits no description tag at all. An empty
/// one is a worse signal than its absence.
#[test]
fn an_item_without_body_text_emits_no_description_tag() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app, ITEM_TYPE, "Page Meta Test").await;

        let title = format!("No Description {}", Uuid::now_v7().simple());
        let id = create_item(app, ITEM_TYPE, &title, serde_json::json!({})).await;

        let head = head_of(app, &format!("/item/{id}")).await;

        assert!(
            !head.contains(r#"<meta name="description""#),
            "no body text must mean no description tag, was:\n{head}"
        );
        assert!(
            !head.contains(r#"<meta property="og:image""#),
            "no image block must mean no og:image, was:\n{head}"
        );
        assert!(
            head.contains(r#"<meta name="twitter:card" content="summary">"#),
            "without an image the card is a plain summary, was:\n{head}"
        );
        assert!(
            head.contains(&format!(r#"<meta property="og:title" content="{title}">"#)),
            "the tags that do have values are still emitted, was:\n{head}"
        );

        delete_item(app, id).await;
    });
}

/// Article-typed items say so, with the timestamps Open Graph defines for them.
#[test]
fn an_article_item_gets_article_og_type_and_timestamps() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app, ARTICLE_TYPE, "Blog").await;

        let title = format!("Article Metadata {}", Uuid::now_v7().simple());
        let id = create_item(
            app,
            ARTICLE_TYPE,
            &title,
            serde_json::json!({"field_body": {"value": "An article."}}),
        )
        .await;

        let head = head_of(app, &format!("/item/{id}")).await;

        assert!(
            head.contains(r#"<meta property="og:type" content="article">"#),
            "a blog item is an article, was:\n{head}"
        );
        assert!(
            head.contains(r#"<meta property="article:published_time""#),
            "an article carries its publication time, was:\n{head}"
        );
        assert!(
            head.contains(r#"<meta property="article:modified_time""#),
            "an article carries its modification time, was:\n{head}"
        );

        delete_item(app, id).await;
    });
}

/// Body text is escaped exactly once on the way into an attribute.
///
/// Stripping tags round-trips through an HTML serializer, so `&` comes back as
/// `&amp;`; Tera then escapes the value again. Without decoding in between, the
/// description ships `&amp;amp;` and a quote in the text would end the
/// attribute.
#[test]
fn body_text_is_escaped_exactly_once_in_the_description() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app, ITEM_TYPE, "Page Meta Test").await;

        let title = format!("Escaping {}", Uuid::now_v7().simple());
        let id = create_item(
            app,
            ITEM_TYPE,
            &title,
            serde_json::json!({
                "field_body": {"value": r#"<p>Salt &amp; pepper, "quoted" &lt;tags&gt;</p>"#},
            }),
        )
        .await;

        let head = head_of(app, &format!("/item/{id}")).await;

        assert!(
            head.contains(
                r#"<meta name="description" content="Salt &amp; pepper, &quot;quoted&quot; &lt;tags&gt;">"#
            ),
            "the description must be escaped once, was:\n{head}"
        );
        assert!(
            !head.contains("&amp;amp;"),
            "double-escaped entity in the head:\n{head}"
        );

        delete_item(app, id).await;
    });
}
