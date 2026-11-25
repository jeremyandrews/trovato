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

/// The config menu cache is process-wide and filled by whichever render reads
/// it first after a write. A render that started reading before a test wrote
/// its menu link can fill the cache after the write's invalidation, without
/// the link, for the cache's whole TTL. Every test here writes menu links and then reads them
/// off a rendered page, so they take turns, each through its cleanup.
static MENUS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        .replace(
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
    // `item_translation` is the translation plugin's table, which the shared
    // app does not create. Without this the file passed only when an earlier
    // target in the same database had migrated the plugin, which is shard
    // order, not a fixture.
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
    // Raw SQL, past the model that would invalidate the menu cache, and a
    // translation changes a menu entry's label and address.
    trovato_kernel::menu::navigation::invalidate_config_menus();
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
    // The link deletes above are raw SQL too.
    trovato_kernel::menu::navigation::invalidate_config_menus();
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
        common::hold_until_cleanup(MENUS.lock().await);
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
        common::hold_until_cleanup(MENUS.lock().await);
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
        common::hold_until_cleanup(MENUS.lock().await);
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
        common::hold_until_cleanup(MENUS.lock().await);
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
        common::hold_until_cleanup(MENUS.lock().await);
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

/// Register a gather query this file owns, listing its own item type.
///
/// Built from JSON rather than the struct literals so that a new field with a
/// serde default does not turn this fixture into a compile error in a file that
/// is not testing gather definitions.
async fn register_gather_query(app: &TestApp, query_id: &str) {
    let definition = serde_json::from_value(serde_json::json!({
        "base_table": "item",
        "item_type": ITEM_TYPE,
        "fields": [],
        "filters": [],
        "sorts": [],
        "relationships": [],
        "includes": {},
        "stage_aware": true,
    }))
    .expect("build gather definition");

    let display = serde_json::from_value(serde_json::json!({
        "format": "table",
        "items_per_page": 10,
        "pager": { "enabled": true, "style": "full", "show_count": true },
        "empty_text": null,
        "header": null,
        "footer": null,
        "canonical_url": null,
    }))
    .expect("build gather display");

    app.state
        .gather()
        .register_query(trovato_kernel::gather::GatherQuery {
            query_id: query_id.to_string(),
            label: "Language Nav Gather".to_string(),
            description: None,
            definition,
            display,
            plugin: "core".to_string(),
            created: 0,
            changed: 0,
        })
        .await
        .expect("register gather query");
}

/// The address the gather is actually read at, in both languages.
///
/// A gather page in a non-default language is reached through an alias: a
/// language-prefixed path is resolved by the alias fallback, and that lookup is
/// language-scoped, so a page readable at `/it/{slug}` needs an Italian row of
/// its own. This is the same shape `create_page` gives an item.
async fn alias_gather(app: &TestApp, query_id: &str, slug: &str) -> String {
    let alias = format!("/{slug}");
    for language in ["en", "it"] {
        UrlAlias::create(
            &app.db,
            CreateUrlAlias {
                source: format!("/gather/{query_id}"),
                alias: alias.clone(),
                language: Some(language.to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create gather alias");
    }
    alias
}

/// A gather page is a page, and its navigation follows the language too.
///
/// `execute_and_render` set `active_language` *after* calling
/// `inject_site_context`, which is where the menus are localized. The value
/// still reached `<html lang>`, so an Italian gather page shipped English
/// navigation under `lang="it"`: the one combination that tells a reader and a
/// screen reader different things about the same page.
///
/// The menu link points at an ordinary item rather than at the gather. What is
/// pinned is not how a gather is linked, it is that the navigation *on* a gather
/// page is built for the language that page is served in.
#[test]
fn a_gather_page_localizes_its_menus() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(MENUS.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let title = format!("Gather Nav {marker}");
        let mut page = create_page(app, &format!("gnav-{marker}"), &title).await;
        let translated = format!("Navigazione {marker}");
        translate(app, page.id, "it", &translated).await;
        // The label mirrors the page title, which is the case worth translating.
        add_menu_link(app, &mut page, &title).await;

        let query_id = format!("lang_nav_{marker}");
        register_gather_query(app, &query_id).await;
        let gather_alias = alias_gather(app, &query_id, &format!("glist-{marker}")).await;

        let (status, html) = get(app, &format!("/it{gather_alias}")).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");

        assert!(
            html.contains(r#"lang="it""#),
            "the gather page must declare the language it was served in:\n{html}"
        );
        assert!(
            html.contains(&format!("href=\"/it{}\"", page.alias)),
            "a menu address on a gather page must move into the language:\n{html}"
        );
        assert!(
            html.contains(&translated),
            "a menu label on a gather page must be the translated title:\n{html}"
        );

        sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/gather/{query_id}"))
            .execute(&app.db)
            .await
            .ok();
        app.state.gather().delete_query(&query_id).await.ok();
        cleanup(app, &[page]).await;
    });
}

/// A main-menu link to any address, outside the page fixtures: a gather alias,
/// an external URL, a route language negotiation does not serve per language.
/// Deleted when the test ends, pass or fail.
async fn add_raw_menu_link(app: &TestApp, path: &str, title: &str) {
    let link = MenuLink::create(
        &app.db,
        CreateMenuLink {
            menu_name: Some("main".to_string()),
            path: path.to_string(),
            title: title.to_string(),
            parent_id: None,
            weight: Some(0),
            hidden: Some(false),
            plugin: Some("core".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create menu link")
    .id;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM menu_link WHERE id = $1")
            .bind(link)
            .execute(&db)
            .await
            .ok();
        trovato_kernel::menu::navigation::invalidate_config_menus();
    });
}

/// A gather this test owns, aliased in both languages, removed when the test
/// ends. Returns the alias it is read at.
async fn owned_gather(app: &TestApp, query_id: &str, slug: &str) -> String {
    register_gather_query(app, query_id).await;
    let alias = alias_gather(app, query_id, slug).await;
    let query_id = query_id.to_string();
    common::defer_cleanup(move || async move {
        let app = shared_app().await;
        sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/gather/{query_id}"))
            .execute(&app.db)
            .await
            .ok();
        app.state.gather().delete_query(&query_id).await.ok();
    });
    alias
}

/// A link to a gather page is a link to a page that exists in every language,
/// so on a page served in Italian it carries the Italian prefix, and on the
/// gather's own Italian page it is the active one.
///
/// The menu kept `/recipes` on `/it/recipes`: the click left the language, and
/// `/recipes` never matched the address asked for, so no link was active.
#[test]
fn a_gather_menu_link_keeps_the_language_and_is_active() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(MENUS.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let gather_alias = owned_gather(
            app,
            &format!("lang_nav_link_{marker}"),
            &format!("recipes-{marker}"),
        )
        .await;
        add_raw_menu_link(app, &gather_alias, &format!("Recipes {marker}")).await;

        let (status, html) = get(app, &format!("/it{gather_alias}")).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let tag = anchor_with_href(&html, &format!("/it{gather_alias}"));
        assert!(
            tag.contains("aria-current=\"page\""),
            "the gather's own link must be the active one, tag was:\n{tag}"
        );
    });
}

/// An item with no translation keeps today's unprefixed address, and is still
/// the active link when its page is read under the Italian prefix: active is
/// decided on the address without its language, on both sides.
#[test]
fn an_untranslated_item_link_is_active_on_its_prefixed_page() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(MENUS.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let title = format!("Plain Active {marker}");
        let mut page = create_page(app, &format!("plain-active-{marker}"), &title).await;
        add_menu_link(app, &mut page, &title).await;
        let (id, alias, link) = (page.id, page.alias.clone(), page.link);
        common::defer_cleanup(move || async move {
            cleanup(shared_app().await, &[Page { id, alias, link }]).await;
        });

        let (status, html) = get(app, &format!("/it{}", page.alias)).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let tag = anchor_with_href(&html, &page.alias);
        assert!(
            tag.contains("aria-current=\"page\""),
            "the page's own link must be active under the prefix, tag was:\n{tag}"
        );
    });
}

/// What is not served per language keeps its address: an external URL, and a
/// path language negotiation passes through untouched (`/api`, `/static`).
#[test]
fn external_and_language_neutral_links_are_left_unprefixed() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(MENUS.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let gather_alias = owned_gather(
            app,
            &format!("lang_nav_neutral_{marker}"),
            &format!("neutral-{marker}"),
        )
        .await;

        let external = format!("https://example.com/{marker}");
        let api = format!("/api/v1/{marker}");
        let file = format!("/static/{marker}.pdf");
        add_raw_menu_link(app, &external, &format!("External {marker}")).await;
        add_raw_menu_link(app, &api, &format!("Api {marker}")).await;
        add_raw_menu_link(app, &file, &format!("Static {marker}")).await;

        let (status, html) = get(app, &format!("/it{gather_alias}")).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        for href in [&external, &api, &file] {
            anchor_with_href(&html, href);
            assert!(
                !html.contains(&format!("href=\"/it{href}\"")),
                "{href} is not served per language and must not be prefixed:\n{html}"
            );
        }
    });
}

/// The default language is untouched: no prefix, and the gather's link is
/// active on its own page exactly as before.
#[test]
fn default_language_menu_links_are_unchanged() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(MENUS.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let gather_alias = owned_gather(
            app,
            &format!("lang_nav_default_{marker}"),
            &format!("dflt-{marker}"),
        )
        .await;
        let external = format!("https://example.com/{marker}");
        add_raw_menu_link(app, &gather_alias, &format!("Default {marker}")).await;
        add_raw_menu_link(app, &external, &format!("External {marker}")).await;

        let (status, html) = get(app, &gather_alias).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let tag = anchor_with_href(&html, &gather_alias);
        assert!(
            tag.contains("aria-current=\"page\""),
            "the gather's link is active on its page, tag was:\n{tag}"
        );
        anchor_with_href(&html, &external);
        assert!(
            !html.contains(&format!("href=\"/it{gather_alias}\"")),
            "a default-language page carries no language prefix:\n{html}"
        );
    });
}

/// A link to an untranslated item with a fragment is still a link to that
/// item: it keeps today's unprefixed address, fragment and all, rather than
/// being taken for a page that exists in every language.
#[test]
fn an_untranslated_item_link_with_a_fragment_keeps_its_address() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(MENUS.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let page = create_page(app, &format!("frag-{marker}"), &format!("Frag {marker}")).await;
        let (id, alias) = (page.id, page.alias.clone());
        common::defer_cleanup(move || async move {
            cleanup(
                shared_app().await,
                &[Page {
                    id,
                    alias,
                    link: None,
                }],
            )
            .await;
        });
        let href = format!("{}#details", page.alias);
        add_raw_menu_link(app, &href, &format!("Frag link {marker}")).await;

        let (status, html) = get(app, &format!("/it{}", page.alias)).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let tag = anchor_with_href(&html, &href);
        assert!(
            tag.contains("aria-current=\"page\""),
            "the link is still the page being read, tag was:\n{tag}"
        );
        assert!(
            !html.contains(&format!("href=\"/it{href}\"")),
            "an untranslated item must not be offered under the prefix:\n{html}"
        );
    });
}

/// A link that names another language is a different page. On English
/// `/recipes`, a menu link to `/it/recipes` is not the current one; only the
/// link to `/recipes` is.
#[test]
fn a_link_into_another_language_is_not_active() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(MENUS.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let gather_alias = owned_gather(
            app,
            &format!("lang_nav_other_{marker}"),
            &format!("other-{marker}"),
        )
        .await;
        let italian = format!("/it{gather_alias}");
        add_raw_menu_link(app, &gather_alias, &format!("Here {marker}")).await;
        add_raw_menu_link(app, &italian, &format!("In Italian {marker}")).await;

        let (status, html) = get(app, &gather_alias).await;
        assert_eq!(status, StatusCode::OK, "body:\n{html}");
        let here = anchor_with_href(&html, &gather_alias);
        assert!(
            here.contains("aria-current=\"page\""),
            "the page's own link is active, tag was:\n{here}"
        );
        let other = anchor_with_href(&html, &italian);
        assert!(
            !other.contains("aria-current"),
            "a link into Italian is not the English page, tag was:\n{other}"
        );
    });
}
