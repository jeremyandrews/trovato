#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A `language_switcher` tile (B176): one link per enabled language, to this
//! same page in that language, the current one marked.
//!
//! Each test places the tile in the sidebar, drives a real page through the
//! router, and reads the tile back off it — the same approach
//! `gather_tile_test.rs` uses. Every item, alias and tile a test makes is
//! removed when it ends: the database is shared with every other test file.
//!
//! The "hidden on a single-language site" half of this feature has no
//! integration test here: every `TestApp` the shared harness builds seeds the
//! same two extra languages (`it`, `he`) before construction
//! (`common::EXTRA_TEST_LANGUAGES`), so no fixture in this binary can produce
//! a one-language `AppState`. That gating is unit-tested directly in
//! `services::tile::tests` instead, on the same function
//! (`language_switcher_links`) this file's requests exercise through the
//! router.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use common::{TestApp, defer_cleanup, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::tile::{CreateTile, Tile};
use trovato_kernel::models::{CreateItem, CreateUrlAlias, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_1A46_0000_00B1;

const ITEM_TYPE: &str = "language_switcher_tile_test";

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
         VALUES ($1, 'Language Switcher Tile Test', 'Fixture type for the language switcher tile', true, 'Title', 'core', $2) \
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
            "Language Switcher Tile Test",
            Some("Fixture type for the language switcher tile"),
            settings,
        )
        .await
        .ok();
}

/// A unique marker for this test's slugs and machine names.
fn stamp() -> String {
    format!("m{}", Uuid::now_v7().simple())
}

/// A published item, with an alias for each of `languages`. Removed when the
/// test ends.
///
/// The alias row carries the same text in every language — the shape
/// `available_translations` assumes (crates/kernel/src/routes/helpers.rs),
/// and the one `language_navigation_test.rs` already pins. What makes it the
/// item's address "in" a language is the row, not different text.
async fn page(app: &TestApp, slug: &str, title: &str, languages: &[&str]) -> (Uuid, String) {
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
                log: Some("language switcher tile test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;

    let alias = format!("/{slug}");
    for language in languages {
        UrlAlias::create(
            &app.db,
            CreateUrlAlias {
                source: format!("/item/{id}"),
                alias: alias.clone(),
                language: Some(language.to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create alias");
    }

    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item_translation WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });

    (id, alias)
}

async fn translate(app: &TestApp, item: Uuid, language: &str, title: &str) {
    common::ensure_translation_table(app);
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

/// Place a `language_switcher` tile in the sidebar. Removed when the test ends.
async fn switcher_tile(app: &TestApp, machine_name: &str) -> Uuid {
    let tile = Tile::create(
        &app.db,
        CreateTile {
            machine_name: machine_name.to_string(),
            label: "Language".to_string(),
            region: Some("sidebar".to_string()),
            tile_type: Some("language_switcher".to_string()),
            config: Some(serde_json::json!({})),
            visibility: Some(serde_json::json!({})),
            weight: Some(0),
            status: Some(1),
            plugin: None,
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create tile");
    let pool = app.db.clone();
    let id = tile.id;
    defer_cleanup(move || async move {
        let _ = Tile::delete(&pool, id).await;
    });
    id
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    // Tera autoescapes `/` to `&#x2F;` in an attribute; nothing here is
    // asserting about escaping, so put the slashes back for readable
    // assertions on addresses.
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, path: &str) -> (StatusCode, String) {
    let response = app
        .request(Request::get(path).body(Body::empty()).unwrap())
        .await;
    let status = response.status();
    (status, body_text(response).await)
}

/// The markup of the tile `machine_name`: up to the next tile, or the end of
/// the sidebar.
fn tile_html<'a>(html: &'a str, machine_name: &str) -> Option<&'a str> {
    let start = html.find(&format!("id=\"tile-{machine_name}\""))?;
    let rest = &html[start..];
    let next_tile = rest[1..].find("id=\"tile-").map(|i| i + 1);
    let end = next_tile
        .or_else(|| rest.find("</aside>"))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The `<a>` tag with this href, so an assertion can be made about one link.
fn anchor_with_href<'a>(html: &'a str, href: &str) -> &'a str {
    let needle = format!("<a href=\"{href}\"");
    let at = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no link to {href} in:\n{html}"));
    let end = html[at..]
        .find('>')
        .map(|e| at + e + 1)
        .unwrap_or(html.len());
    &html[at..end]
}

#[test]
fn one_link_per_enabled_language_the_current_one_marked_and_the_translation_used() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = stamp();
        let (id, alias) = page(
            app,
            &format!("switcher-{marker}"),
            &format!("Switcher {marker}"),
            &["en", "it"],
        )
        .await;
        translate(app, id, "it", &format!("Interruttore {marker}")).await;
        let machine = format!("lswitch_{marker}");
        switcher_tile(app, &machine).await;

        // English: the default language, read at the plain alias.
        let (status, html) = get(app, &alias).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let tile = tile_html(&html, &machine).unwrap_or_else(|| panic!("no tile:\n{html}"));

        let en = anchor_with_href(tile, &alias);
        assert!(
            en.contains("aria-current=\"true\""),
            "English is the page being read: {en}"
        );
        assert!(en.contains("hreflang=\"en\""), "{en}");
        assert!(en.contains("lang=\"en\""), "{en}");

        let it_href = format!("/it{alias}");
        let it = anchor_with_href(tile, &it_href);
        assert!(
            !it.contains("aria-current"),
            "Italian is not the page being read: {it}"
        );
        assert!(it.contains("hreflang=\"it\""), "{it}");

        let he = anchor_with_href(tile, "/he");
        assert!(
            !he.contains("aria-current"),
            "Hebrew has no translation, so it is not current either: {he}"
        );

        // Italian: the same page, reached through its Italian alias row. The
        // English link now carries the plain (unprefixed) address, and the
        // Italian one is the page being read.
        let (status, html) = get(app, &it_href).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let tile = tile_html(&html, &machine).unwrap_or_else(|| panic!("no tile:\n{html}"));

        let en = anchor_with_href(tile, &alias);
        assert!(
            !en.contains("aria-current"),
            "English is not the page being read now: {en}"
        );
        let it = anchor_with_href(tile, &it_href);
        assert!(
            it.contains("aria-current=\"true\""),
            "Italian is the page being read: {it}"
        );
    });
}

#[test]
fn a_language_with_no_translation_falls_back_to_its_front_page() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = stamp();
        let (_id, alias) = page(
            app,
            &format!("notrans-{marker}"),
            &format!("No Translation {marker}"),
            &["en"],
        )
        .await;
        let machine = format!("lswitch_nt_{marker}");
        switcher_tile(app, &machine).await;

        let (status, html) = get(app, &alias).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let tile = tile_html(&html, &machine).unwrap_or_else(|| panic!("no tile:\n{html}"));

        // Neither `it` nor `he` has a translation of this item: both fall
        // back to their own front page rather than to a 404.
        let it = anchor_with_href(tile, "/it");
        assert!(!it.contains("aria-current"), "{it}");
        let he = anchor_with_href(tile, "/he");
        assert!(!he.contains("aria-current"), "{he}");
    });
}
