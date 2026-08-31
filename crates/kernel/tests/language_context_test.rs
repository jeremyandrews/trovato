#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The language a page is served in has to reach the page.
//!
//! `inject_site_context` supplies a *default* `active_language`; the route
//! supplies the *actual* one. The helper used to insert its default
//! unconditionally, so which value survived depended on whether the route
//! happened to insert before or after calling it. The item route inserted
//! before, and every translated item page it served claimed to be in the site's
//! default language: wrong `<html lang>`, wrong `dir`, and nothing a template
//! could branch on.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use common::{TestApp, run_test, shared_app};
use tower_sessions::{MemoryStore, Session};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding, for the same
/// reason `common`'s conference seeder takes one: check-then-insert against a
/// database shared by every test process.
const TYPE_SEED_LOCK: i64 = 0x_1A46_0000_0001;

/// The item type these tests create content with.
const ITEM_TYPE: &str = "language_context_test";

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
         VALUES ($1, 'Language Context Test', 'Fixture type for language context tests', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed language context test item type");

    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .create(
            ITEM_TYPE,
            "Language Context Test",
            Some("Fixture type for language context tests"),
            settings,
        )
        .await
        .ok();
}

/// Create a published item with the given default-language title.
async fn create_item(app: &TestApp, title: &str) -> Uuid {
    app.state
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
                log: Some("language context test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id
}

/// Record a translation of `item` into `language`.
async fn translate(app: &TestApp, item: Uuid, language: &str, title: &str) {
    sqlx::query(
        "INSERT INTO item_translation (item_id, language, title, fields) \
         VALUES ($1, $2, $3, '{}'::jsonb) \
         ON CONFLICT (item_id, language) DO UPDATE SET title = EXCLUDED.title",
    )
    .bind(item)
    .bind(language)
    .bind(title)
    .execute(&app.db)
    .await
    .expect("record translation");
}

/// Ask the item route for `id` in `language`.
///
/// Through `Accept-Language` rather than a `/{lang}/` prefix: a prefix is only
/// stripped by the alias fallback, so it reaches the item route only for items
/// that have a URL alias. The header is the negotiator that puts a non-default
/// `ResolvedLanguage` in front of this route with nothing else in the way,
/// which is the seam under test.
fn item_request(id: Uuid, language: &str) -> Request<Body> {
    Request::get(format!("/item/{id}"))
        .header("Accept-Language", language)
        .body(Body::empty())
        .unwrap()
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A `Session` with no request behind it, for calling context helpers directly.
///
/// Backed by an in-memory store because nothing is meant to be read out of it:
/// the helper only asks whether anyone is logged in, and nobody is.
fn detached_session() -> Session {
    Session::new(None, Arc::new(MemoryStore::default()), None)
}

/// An item served under a language prefix renders as that language, not as the
/// site default, and carries the translated content.
///
/// This is the bug in one assertion: `lang="en"` on a page of Italian prose is
/// what a screen reader acts on (WCAG 3.1.1), and it is what the item route
/// emitted for every translated page.
#[test]
fn a_translated_item_page_is_served_in_its_own_language() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let id = create_item(app, &format!("Default title {marker}")).await;
        let translated = format!("Titolo tradotto {marker}");
        translate(app, id, "it", &translated).await;

        let response = app.request(item_request(id, "it")).await;
        let status = response.status();
        let html = body_text(response).await;
        assert_eq!(status, StatusCode::OK, "body: {html}");

        assert!(
            html.contains(r#"lang="it""#),
            "page served under /it must declare lang=\"it\": {}",
            &html[..html.len().min(400)]
        );
        assert!(
            html.contains(&translated),
            "the translated title must be on the page"
        );

        app.state.items().delete(id, &admin()).await.ok();
    });
}

/// The direction travels with the language. A right-to-left language reaching
/// the route has to reach `<html dir>` too — `text_direction` was overwritten by
/// exactly the same unconditional insert.
#[test]
fn a_right_to_left_language_reaches_the_page_direction() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let id = create_item(app, &format!("Default title {marker}")).await;
        translate(app, id, "he", &format!("כותרת {marker}")).await;

        let response = app.request(item_request(id, "he")).await;
        let html = body_text(response).await;

        assert!(html.contains(r#"lang="he""#), "expected lang=\"he\"");
        assert!(
            html.contains(r#"dir="rtl""#),
            "a right-to-left language must set dir=\"rtl\""
        );

        app.state.items().delete(id, &admin()).await.ok();
    });
}

/// The regression test for the helper itself: a language already in the context
/// belongs to the route and is left alone.
///
/// `text_direction` is deliberately set to something the language does not imply
/// so that a helper which recomputed rather than deferred would be caught too.
#[test]
fn inject_site_context_does_not_overwrite_the_routes_language() {
    run_test(async {
        let app = shared_app().await;

        let mut context = tera::Context::new();
        context.insert("active_language", "it");
        context.insert("text_direction", "rtl");

        trovato_kernel::routes::helpers::inject_site_context(
            &app.state,
            &detached_session(),
            &mut context,
            "/",
        )
        .await;

        assert_eq!(
            context.get("active_language").and_then(|v| v.as_str()),
            Some("it"),
            "the route's language must survive the helper"
        );
        assert_eq!(
            context.get("text_direction").and_then(|v| v.as_str()),
            Some("rtl"),
            "the route's direction must survive the helper"
        );
        assert_eq!(
            context.get("default_language").and_then(|v| v.as_str()),
            Some(app.state.default_language()),
            "the site default is still reported, under its own name"
        );
    });
}

/// The other half of the contract: a route that says nothing still gets a
/// language, so no page renders without one.
#[test]
fn inject_site_context_supplies_a_default_when_the_route_is_silent() {
    run_test(async {
        let app = shared_app().await;

        let mut context = tera::Context::new();
        trovato_kernel::routes::helpers::inject_site_context(
            &app.state,
            &detached_session(),
            &mut context,
            "/",
        )
        .await;

        assert_eq!(
            context.get("active_language").and_then(|v| v.as_str()),
            Some(app.state.default_language())
        );
        assert_eq!(
            context.get("text_direction").and_then(|v| v.as_str()),
            Some("ltr")
        );
    });
}
