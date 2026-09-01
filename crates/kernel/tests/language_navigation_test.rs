#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Keeping a reader inside a translation.
//!
//! A site serving `/it/why` used to render its navigation with default-language
//! addresses and default-language labels, offer nothing a language switcher could
//! be built from, and emit no `hreflang` alternates. Every fact needed for all
//! three was already in the kernel. These tests pin what it now does with them.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateMenuLink, CreateUrlAlias, MenuLink, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_1A46_0000_0002;

const ITEM_TYPE: &str = "language_nav_test";

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
         VALUES ($1, 'Language Nav Test', 'Fixture type for language navigation tests', true, 'Title', 'core', $2) \
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
        .create(
            ITEM_TYPE,
            "Language Nav Test",
            Some("Fixture type for language navigation tests"),
            settings,
        )
        .await
        .ok();
}

/// A published item, its alias in both languages, and a main-menu link to it.
///
/// The alias is registered per language because that is what serving `/it/why`
/// takes: the alias fallback looks a path up in the active language, so a page
/// reachable in Italian needs an Italian row. The two rows carry the same alias,
/// which is the shape `available_translations` assumes.
struct Page {
    id: Uuid,
    alias: String,
    link: Option<Uuid>,
}

async fn create_page(app: &TestApp, slug: &str, title: &str) -> Page {
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
                log: Some("language nav test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;

    let alias = format!("/{slug}");
    for language in ["en", "it"] {
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

    Page {
        id,
        alias,
        link: None,
    }
}

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

async fn add_menu_link(app: &TestApp, page: &mut Page, title: &str) {
    let link = MenuLink::create(
        &app.db,
        CreateMenuLink {
            menu_name: Some("main".to_string()),
            path: page.alias.clone(),
            title: title.to_string(),
            parent_id: None,
            weight: Some(0),
            hidden: Some(false),
            plugin: Some("core".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create menu link");
    page.link = Some(link.id);
}

async fn cleanup(app: &TestApp, pages: &[Page]) {
    for page in pages {
        if let Some(link) = page.link {
            sqlx::query("DELETE FROM menu_link WHERE id = $1")
                .bind(link)
                .execute(&app.db)
                .await
                .ok();
        }
        sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{}", page.id))
            .execute(&app.db)
            .await
            .ok();
        app.state.items().delete(page.id, &admin()).await.ok();
    }
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn get(app: &TestApp, path: &str) -> (StatusCode, String) {
    let response = app
        .request(Request::get(path).body(Body::empty()).unwrap())
        .await;
    let status = response.status();
    (status, readable(&body_text(response).await))
}

/// Put the slashes back so an assertion can be written in addresses.
///
/// Tera autoescapes `/` to `&#x2F;` in every attribute it writes, which is
/// correct HTML and unreadable in a test. Nothing here is asserting about
/// escaping; the assertions are about which address a link carries.
fn readable(html: &str) -> String {
    html.replace("&#x2F;", "/")
}

/// The `<a>` tag with this href, so an assertion can be made about one link
/// rather than about the whole page.
///
/// An anchor specifically: the same address appears in the `hreflang` alternates
/// in the head, and a bare substring search finds those first.
fn anchor_with_href<'a>(html: &'a str, href: &str) -> &'a str {
    let needle = format!("<a href=\"{href}\"");
    let at = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no link to {href} on the page:\n{html}"));
    let end = html[at..]
        .find('>')
        .map(|e| at + e + 1)
        .unwrap_or(html.len());
    &html[at..end]
}

/// A menu entry pointing at a page that exists in the active language is
/// rewritten to that language, labelled in it, and recognised as the page being
/// read.
///
/// Three separate gaps in one request, because they are one behaviour to a
/// reader: the click stays in Italian, the label is Italian, and the menu knows
/// where you are. The last is `requested_path` doing its job — `current_path` on
/// this page is `/item/{uuid}`, which no menu link can ever equal.
#[test]
fn a_translated_menu_entry_is_prefixed_labelled_and_current() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let title = format!("Why Trovato {marker}");
        let mut page = create_page(app, &format!("why-{marker}"), &title).await;
        let translated = format!("Perche Trovato {marker}");
        translate(app, page.id, "it", &translated).await;
        // The label mirrors the page title, which is the case worth translating.
        add_menu_link(app, &mut page, &title).await;

        let (status, html) = get(app, &format!("/it{}", page.alias)).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");

        let expected_href = format!("/it{}", page.alias);
        let link_tag = anchor_with_href(&html, &expected_href);
        assert!(
            link_tag.contains("aria-current=\"page\""),
            "the menu link for the page being read must be the current one, tag was:\n{link_tag}"
        );
        assert!(
            html.contains(&translated),
            "the menu label must be the translated title:\n{html}"
        );

        cleanup(app, &[page]).await;
    });
}

/// A menu entry whose target has no translation keeps its address and its label.
///
/// A translated label on an untranslated page is a promise the click breaks, and
/// a prefixed address for a page that does not exist in that language is a 404
/// offered as navigation.
#[test]
fn a_menu_entry_with_no_translation_is_left_alone() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let translated_title = format!("Translated {marker}");
        let mut translated_page =
            create_page(app, &format!("tr-{marker}"), &translated_title).await;
        translate(app, translated_page.id, "it", &format!("Tradotto {marker}")).await;

        let plain_title = format!("Plain {marker}");
        let mut plain_page = create_page(app, &format!("plain-{marker}"), &plain_title).await;
        add_menu_link(app, &mut translated_page, &translated_title).await;
        add_menu_link(app, &mut plain_page, &plain_title).await;

        let (status, html) = get(app, &format!("/it{}", translated_page.alias)).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");

        let plain_tag = anchor_with_href(&html, &plain_page.alias);
        assert!(
            !plain_tag.contains("/it/"),
            "an untranslated target keeps its address, tag was:\n{plain_tag}"
        );
        assert!(
            html.contains(&plain_title),
            "an untranslated target keeps its label:\n{html}"
        );

        cleanup(app, &[translated_page, plain_page]).await;
    });
}

/// A label somebody wrote by hand is theirs. The address still moves into the
/// language, because that page does exist there.
#[test]
fn a_hand_written_menu_label_survives_translation() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let title = format!("Documentation {marker}");
        let mut page = create_page(app, &format!("docs-{marker}"), &title).await;
        translate(app, page.id, "it", &format!("Documentazione {marker}")).await;

        let hand_written = format!("Read the docs {marker}");
        add_menu_link(app, &mut page, &hand_written).await;

        let (status, html) = get(app, &format!("/it{}", page.alias)).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");

        let expected_href = format!("/it{}", page.alias);
        assert!(
            html.contains(&format!("href=\"{expected_href}\"")),
            "the address moves into the language:\n{html}"
        );
        assert!(
            html.contains(&hand_written),
            "a label that is not the page title is left as written:\n{html}"
        );

        cleanup(app, &[page]).await;
    });
}

/// Every language a page can be read in, with its address, default included.
#[test]
fn available_translations_lists_the_languages_the_page_exists_in() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let page = create_page(app, &format!("avail-{marker}"), &format!("Avail {marker}")).await;

        // With no translation, only the default language is on offer.
        let alone = trovato_kernel::routes::helpers::available_translations(
            &app.state,
            page.id,
            &page.alias,
        )
        .await;
        assert_eq!(alone.len(), 1);
        assert_eq!(alone[0].language, app.state.default_language());
        assert_eq!(alone[0].path, page.alias);

        translate(app, page.id, "it", &format!("Disponibile {marker}")).await;

        let both = trovato_kernel::routes::helpers::available_translations(
            &app.state,
            page.id,
            &page.alias,
        )
        .await;
        assert_eq!(both.len(), 2, "default language plus the translation");
        assert_eq!(both[1].language, "it");
        assert_eq!(
            both[1].path,
            format!("/it{}", page.alias),
            "a non-default language is the same address behind its prefix"
        );

        cleanup(app, &[page]).await;
    });
}

/// `hreflang` alternates are emitted for a page that has a translation, and for a
/// page that does not they are absent rather than self-referential.
#[test]
fn hreflang_tags_name_the_translations_and_nothing_else() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let translated_page =
            create_page(app, &format!("hl-{marker}"), &format!("Hreflang {marker}")).await;
        translate(
            app,
            translated_page.id,
            "it",
            &format!("Hreflang IT {marker}"),
        )
        .await;
        let plain_page = create_page(
            app,
            &format!("hlp-{marker}"),
            &format!("No Hreflang {marker}"),
        )
        .await;

        let (status, html) = get(app, &translated_page.alias).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!(
                "hreflang=\"it\" href=\"/it{}\"",
                translated_page.alias
            )),
            "the Italian alternate must name the Italian address:\n{html}"
        );
        assert!(
            html.contains("hreflang=\"x-default\""),
            "x-default must be emitted alongside:\n{html}"
        );

        let (status, html) = get(app, &plain_page.alias).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !html.contains("hreflang="),
            "one language is nothing to alternate between:\n{html}"
        );

        cleanup(app, &[translated_page, plain_page]).await;
    });
}
