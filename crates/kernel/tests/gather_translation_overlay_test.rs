#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A gather row shows the translation's field values, not the original's.
//!
//! `query_builder.rs` merged a row's `fields` with
//! `COALESCE(_tr.fields || item.fields, item.fields)`. On PostgreSQL JSONB,
//! `a || b` lets `b` win on a duplicate key, and `item.fields` was `b`: every
//! key the *original* item had overrode the translation, so a Spanish gather
//! row showed an English body and an English image alt text, with only the
//! title (a separate column) actually translated. This file proves the row a
//! gather returns in a translated language carries the translation's values —
//! body and image alt text both — falling back to the original only for a key
//! the translation does not have.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::collections::HashMap;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter,
};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_1A46_0000_0004;

const ITEM_TYPE: &str = "gather_translation_overlay_test";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::Table,
        items_per_page: Some(50),
        pager: PagerConfig {
            enabled: true,
            style: PagerStyle::Full,
            show_count: true,
            zero_based: false,
        },
        empty_text: None,
        header: None,
        footer: None,
        canonical_url: None,
        routes: Vec::new(),
        feed: None,
    }
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
         VALUES ($1, 'Gather Translation Overlay Test', 'Fixture type', true, 'Title', 'core', $2) \
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
            "Gather Translation Overlay Test",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

/// Run a gather over this test's items only, as a viewer reading `language`
/// (`None` for the site default), and return the first row's `fields`.
async fn first_row_fields(
    app: &TestApp,
    marker: &str,
    language: Option<&str>,
) -> serde_json::Value {
    let definition = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(ITEM_TYPE.to_string()),
        filters: vec![QueryFilter {
            field: "title".to_string(),
            operator: FilterOperator::Contains,
            value: FilterValue::String(marker.to_string()),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }],
        stage_aware: true,
        ..Default::default()
    };

    let ctx = QueryContext {
        current_user_id: None,
        viewer: Some(admin()),
        url_args: HashMap::new(),
        language: language.map(str::to_string),
    };

    let result = app
        .state
        .gather()
        .execute_definition(
            &definition,
            &display(),
            1,
            HashMap::new(),
            LIVE_STAGE_ID,
            &ctx,
        )
        .await
        .expect("gather executes");

    assert_eq!(
        result.items.len(),
        1,
        "exactly this test's one item: {result:?}"
    );
    result.items[0]["fields"].clone()
}

async fn cleanup(app: &TestApp, id: Uuid) {
    app.state.items().delete(id, &admin()).await.ok();
}

/// An item and its Spanish translation differ in `body` and the image's
/// `alt` text, and share an untranslated `category` field. A gather row read
/// in Spanish carries the Spanish body and alt text, falling back to the
/// shared `category` only because the translation never set it; a gather row
/// read in the site's default language is unaffected by the translation at
/// all.
#[test]
fn a_gather_row_shows_the_translations_fields_and_falls_back_for_the_rest() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;
        common::ensure_translation_table(app);
        let marker = format!("trover{}", Uuid::now_v7().simple());

        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: ITEM_TYPE.to_string(),
                    title: format!("{marker} original"),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "body": "the original body",
                        "image": {"src": "/files/banner.png", "alt": "a sunrise over the harbor"},
                        "category": "travel",
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("gather translation overlay test".to_string()),
                },
                &admin(),
            )
            .await
            .expect("create item")
            .id;

        app.state
            .items()
            .save_translation(
                item,
                "es",
                &format!("{marker} original en espanol"),
                &serde_json::json!({
                    "body": "el cuerpo traducido",
                    "image": {"src": "/files/banner.png", "alt": "un amanecer sobre el puerto"},
                }),
                &admin(),
            )
            .await
            .expect("save translation");

        let spanish = first_row_fields(app, &marker, Some("es")).await;
        assert_eq!(
            spanish["body"], "el cuerpo traducido",
            "translated body wins in the translated language: {spanish}"
        );
        assert_eq!(
            spanish["image"]["alt"], "un amanecer sobre el puerto",
            "translated alt text wins in the translated language: {spanish}"
        );
        assert_eq!(
            spanish["category"], "travel",
            "a key the translation never set falls back to the original: {spanish}"
        );

        let original = first_row_fields(app, &marker, None).await;
        assert_eq!(
            original["body"], "the original body",
            "the default-language row is unaffected by the translation: {original}"
        );
        assert_eq!(
            original["image"]["alt"], "a sunrise over the harbor",
            "the default-language row is unaffected by the translation: {original}"
        );

        cleanup(app, item).await;
    });
}
