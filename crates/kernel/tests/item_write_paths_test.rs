#![allow(clippy::unwrap_used, clippy::expect_used)]
//! An item's side effects follow the item, not the screen that wrote it.
//!
//! Pathauto ran from four route handlers, so an item a bulk action, a config
//! import or a plugin saved never got an alias; deleting an item left its
//! aliases and the menu links to it behind; and every alias was written in
//! `en` on the live stage whatever the item's were. These tests write items
//! the ways that are not the edit form and read back what the item service
//! (or, for import, the storage layer beside it) did.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateMenuLink, MenuLink, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// The content type these tests own, so its pathauto pattern is theirs.
const TYPE: &str = "b87b_alias_page";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// Register [`TYPE`] and give it the pattern `b87b/[title]`, merged into the
/// site's patterns so another type's pattern is left alone.
async fn ensure_type_with_pattern(app: &TestApp) {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, 'B87b alias fixture', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(TYPE)
    .execute(&app.db)
    .await
    .expect("seed item type");
    sqlx::query(
        "INSERT INTO site_config (key, value) VALUES ('pathauto_patterns', $1) \
         ON CONFLICT (key) DO UPDATE SET value = site_config.value || EXCLUDED.value",
    )
    .bind(serde_json::json!({ TYPE: "b87b/[title]" }))
    .execute(&app.db)
    .await
    .expect("set the pathauto pattern");
}

fn unique_title(prefix: &str) -> String {
    format!("{prefix} {}", Uuid::now_v7().simple())
}

fn slug(title: &str) -> String {
    format!(
        "/b87b/{}",
        trovato_kernel::services::pathauto::slugify(title)
    )
}

/// Every alias row of an item: `(alias, language, stage)`.
async fn aliases_of(app: &TestApp, id: Uuid) -> Vec<(String, String, Uuid)> {
    sqlx::query_as("SELECT alias, language, stage_id FROM url_alias WHERE source = $1")
        .bind(format!("/item/{id}"))
        .fetch_all(&app.db)
        .await
        .expect("read aliases")
}

/// An item row written straight to the table, so nothing has given it an
/// alias yet.
async fn raw_item(app: &TestApp, title: &str, status: i16) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, stage_id, fields, language) \
         VALUES ($1, $2, $3, $4, $5, 1, 1, $6, '{}'::jsonb, 'en')",
    )
    .bind(id)
    .bind(TYPE)
    .bind(title)
    .bind(status)
    .bind(Uuid::nil())
    .bind(LIVE_STAGE_ID)
    .execute(&app.db)
    .await
    .expect("seed a raw item");
    forget_on_exit(app, id);
    id
}

fn forget_on_exit(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let source = format!("/item/{id}");
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(&source)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

fn token_in(html: &str) -> Option<String> {
    let at = html.find(r#"name="_token""#)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// A bulk publish is `ItemService::update` per item, so the item it publishes
/// gets the alias its pattern asks for.
#[test]
fn a_bulk_action_gives_the_item_its_alias() {
    run_test(async {
        let app = shared_app().await;
        ensure_type_with_pattern(app).await;
        let title = unique_title("bulk alias");
        let id = raw_item(app, &title, 0).await;
        assert!(aliases_of(app, id).await.is_empty(), "no alias to start");

        let (_, cookies) = user_holding(
            app,
            "b87b-bulk",
            &["edit any content", "publish content", "access content"],
        )
        .await;
        let page = app
            .request_with_cookies(
                Request::get("/admin/content")
                    .header("x-forwarded-for", test_ip_for("b87b-bulk"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let token = token_in(&body_of(page).await).expect("the bulk form carries a token");
        let body = format!(
            "_token={}&action=publish&ids%5B%5D={id}",
            urlencoding::encode(&token)
        );
        let response = app
            .request_with_cookies(
                Request::post("/admin/content/bulk")
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .header("x-forwarded-for", test_ip_for("b87b-bulk"))
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        assert_eq!(
            aliases_of(app, id).await,
            vec![(slug(&title), "en".to_string(), LIVE_STAGE_ID)],
            "the bulk-published item must have its pattern's alias"
        );
    });
}

/// Config import writes the item row itself; the alias comes with it.
#[test]
fn a_config_imported_item_gets_its_alias() {
    run_test(async {
        let app = shared_app().await;
        ensure_type_with_pattern(app).await;
        let title = unique_title("import alias");
        let id = Uuid::now_v7();
        forget_on_exit(app, id);
        let item: trovato_kernel::config_storage::ConfigItem =
            serde_json::from_value(serde_json::json!({
                "id": id,
                "type": TYPE,
                "title": title,
                "language": "it",
            }))
            .expect("a config item");
        app.config_storage()
            .save(&trovato_kernel::config_storage::ConfigEntity::Item(item))
            .await
            .expect("import the item");

        assert_eq!(
            aliases_of(app, id).await,
            vec![(slug(&title), "it".to_string(), LIVE_STAGE_ID)],
            "an imported item must get its alias, in its own language"
        );
    });
}

/// An item created with no language is in the site's default language, and
/// so is its alias; an item in another language has its alias in that one,
/// and the address still resolves for a reader in the default language.
#[test]
fn an_alias_is_written_in_the_items_language() {
    run_test(async {
        let app = shared_app().await;
        ensure_type_with_pattern(app).await;

        let mut ids = Vec::new();
        for language in [None, Some("it")] {
            let title = unique_title("language alias");
            let item = app
                .state
                .items()
                .create(
                    CreateItem {
                        item_type: TYPE.to_string(),
                        title: title.clone(),
                        author_id: Uuid::nil(),
                        status: Some(1),
                        promote: Some(0),
                        sticky: Some(0),
                        fields: Some(serde_json::json!({})),
                        stage_id: None,
                        language: language.map(str::to_string),
                        log: None,
                    },
                    &admin(),
                )
                .await
                .expect("create");
            forget_on_exit(app, item.id);
            let expected = language.unwrap_or(app.state.default_language());
            assert_eq!(item.language, expected);
            assert_eq!(
                aliases_of(app, item.id).await,
                vec![(slug(&title), expected.to_string(), LIVE_STAGE_ID)],
            );
            ids.push((item.id, title));
        }

        let (it_id, it_title) = &ids[1];
        let found = UrlAlias::find_by_alias_with_context(
            &app.db,
            &slug(it_title),
            LIVE_STAGE_ID,
            app.state.default_language(),
        )
        .await
        .unwrap()
        .expect("an Italian item's alias resolves for a default-language reader");
        assert_eq!(found.source, format!("/item/{it_id}"));
    });
}

/// Deleting an item takes its aliases and the menu links to it with it, and a
/// link nested under one of those moves up rather than out of its branch.
#[test]
fn deleting_an_item_removes_its_aliases_and_menu_links() {
    run_test(async {
        let app = shared_app().await;
        ensure_type_with_pattern(app).await;
        let title = unique_title("delete alias");
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: TYPE.to_string(),
                    title: title.clone(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("create");
        forget_on_exit(app, item.id);
        assert_eq!(aliases_of(app, item.id).await.len(), 1);

        let menu = format!("b87b-{}", &Uuid::now_v7().simple().to_string()[..12]);
        let link = |path: String, parent_id: Option<Uuid>| CreateMenuLink {
            menu_name: Some(menu.clone()),
            path,
            title: "link".to_string(),
            parent_id,
            weight: None,
            hidden: None,
            plugin: None,
            stage_id: None,
        };
        let top = MenuLink::create(&app.db, link("/about".to_string(), None))
            .await
            .unwrap();
        let by_id = MenuLink::create(&app.db, link(format!("/item/{}", item.id), Some(top.id)))
            .await
            .unwrap();
        let by_alias = MenuLink::create(&app.db, link(slug(&title), None))
            .await
            .unwrap();
        let child = MenuLink::create(&app.db, link("/child".to_string(), Some(by_id.id)))
            .await
            .unwrap();
        let pool = app.db.clone();
        let cleanup_menu = menu.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM menu_link WHERE menu_name = $1")
                .bind(cleanup_menu)
                .execute(&pool)
                .await;
        });

        assert!(app.state.items().delete(item.id, &admin()).await.unwrap());

        assert!(aliases_of(app, item.id).await.is_empty(), "aliases go");
        assert!(
            MenuLink::find_by_id(&app.db, by_id.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            MenuLink::find_by_id(&app.db, by_alias.id)
                .await
                .unwrap()
                .is_none()
        );
        let child = MenuLink::find_by_id(&app.db, child.id)
            .await
            .unwrap()
            .expect("a child of a removed link stays");
        assert_eq!(child.parent_id, Some(top.id), "and moves up a level");
    });
}

/// A title edit through the service regenerates the alias in place.
#[test]
fn an_update_regenerates_the_alias() {
    run_test(async {
        let app = shared_app().await;
        ensure_type_with_pattern(app).await;
        let title = unique_title("rename alias");
        let id = raw_item(app, &title, 1).await;
        let renamed = unique_title("renamed alias");
        app.state
            .items()
            .update(
                id,
                trovato_kernel::models::UpdateItem {
                    author_id: None,
                    created: None,
                    stage_id: None,
                    title: Some(renamed.clone()),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: None,
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("update")
            .expect("the item");
        assert_eq!(
            aliases_of(app, id).await,
            vec![(slug(&renamed), "en".to_string(), LIVE_STAGE_ID)]
        );
    });
}
