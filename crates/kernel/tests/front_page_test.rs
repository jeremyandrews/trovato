#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Front page routing.
//!
//! Two behaviours are pinned here. First, `site_front_page` may name any path
//! the site serves, and `/` answers with exactly what that path renders, no
//! redirect: an item, a gather (paged through `/?page=2`), an alias of one, a
//! plugin route. Access, status and cache headers are the target's; a target
//! that is gone is the stock front page; a path pointing off-site is refused.
//! Second, the default
//! promoted listing asks for promoted items rather than filtering a page of
//! published ones, so a promoted item behind a page of newer published items is
//! still listed.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use common::{TestApp, defer_cleanup, run_test, settle_cache_generations, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateUrlAlias, RegistrationMode, SiteConfig, UrlAlias};
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
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
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
        .replace(
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
    create_item_with_fields(app, title, promote, created, serde_json::json!({})).await
}

/// The same, with field values, for the tests that care what the teaser renders.
async fn create_item_with_fields(
    app: &TestApp,
    title: &str,
    promote: i16,
    created: i64,
    fields: serde_json::Value,
) -> Uuid {
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
                fields: Some(fields),
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
    app.state.front_page().clear();
}

async fn clear_front_page(app: &TestApp) {
    SiteConfig::delete(&app.db, "site_front_page")
        .await
        .expect("clear front page");
    app.state.front_page().clear();
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

/// GET `path` anonymously.
async fn get(app: &TestApp, path: &str) -> Response {
    app.request(Request::get(path).body(Body::empty()).unwrap())
        .await
}

/// The `<link rel="canonical">` href of a page, if it has one.
fn canonical(body: &str) -> Option<String> {
    let start =
        body.find(r#"<link rel="canonical" href=""#)? + r#"<link rel="canonical" href=""#.len();
    let end = body[start..].find('"')?;
    Some(body[start..start + end].to_string())
}

/// The site's own root, as an absolute canonical URL names it.
fn site_root(app: &TestApp) -> String {
    format!("{}/", app.state.runtime().site_url.trim_end_matches('/'))
}

/// The headers a response is cached by, for comparing `/` with its target.
fn cache_headers(response: &Response) -> Vec<(String, String)> {
    [header::CACHE_CONTROL, header::VARY, header::CONTENT_TYPE]
        .iter()
        .filter_map(|name| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| (name.to_string(), v.to_string()))
        })
        .collect()
}

/// Register a gather listing this file's items whose title starts with
/// `marker`, three to a page, sorted by title. Removed when the test ends.
async fn paged_gather(app: &TestApp, marker: &str, permission: Option<&str>) -> String {
    let query_id = format!("front_{}", marker.to_lowercase());
    let mut definition = serde_json::json!({
        "base_table": "item",
        "item_type": ITEM_TYPE,
        "filters": [{ "field": "title", "operator": "starts_with", "value": marker }],
        "sorts": [{ "field": "title", "direction": "asc", "nulls": null }]
    });
    if let Some(permission) = permission {
        definition["access"] = serde_json::json!({ "permission": permission });
    }
    let query: trovato_kernel::gather::types::GatherQuery =
        serde_json::from_value(serde_json::json!({
            "query_id": query_id,
            "label": format!("Front {marker}"),
            "description": "Front page test listing",
            "definition": definition,
            "display": { "format": "table", "items_per_page": 3, "routes": [] },
            "plugin": "core"
        }))
        .expect("gather deserializes");
    app.state
        .gather()
        .register_query(query)
        .await
        .expect("register gather");
    let pool = app.db.clone();
    let id = query_id.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(&id)
            .execute(&pool)
            .await;
    });
    query_id
}

/// Seven items titled `{marker} 1` through `{marker} 7`.
async fn listed_items(app: &TestApp, marker: &str) -> Vec<Uuid> {
    let now = chrono::Utc::now().timestamp();
    let mut ids = Vec::new();
    for n in 1..=7 {
        ids.push(create_item(app, &format!("{marker} {n}"), 0, now).await);
    }
    ids
}

/// An item front page renders at `/`: no redirect, the item's own page, and a
/// canonical link naming `/`.
#[test]
fn front_page_set_to_an_item_renders_it() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let title = format!("Item Front Page {}", Uuid::now_v7().simple());
        let id = create_item(app, &title, 0, chrono::Utc::now().timestamp()).await;
        set_front_page(app, &format!("/item/{id}")).await;

        let front = get_front_page(app).await;
        assert_eq!(front.status(), StatusCode::OK, "item renders at /");
        assert_eq!(location(&front), None, "no redirect");
        let front_headers = cache_headers(&front);
        let body = body_text(front).await;
        assert!(body.contains(&title), "the item is rendered at /:\n{body}");
        assert_eq!(
            canonical(&body),
            Some(site_root(app)),
            "the front page is canonically /"
        );

        // The item's own address still works, with the same headers.
        let target = get(app, &format!("/item/{id}")).await;
        assert_eq!(target.status(), StatusCode::OK);
        assert_eq!(cache_headers(&target), front_headers);
        let target_body = body_text(target).await;
        assert!(target_body.contains(&title));
        assert_ne!(
            canonical(&target_body),
            Some(site_root(app)),
            "at its own address the item is not canonically /"
        );

        clear_front_page(app).await;
        delete_items(app, &[id]).await;
    });
}

/// A gather front page renders at `/`, and `/?page=2` is its second page.
#[test]
fn front_page_set_to_a_gather_renders_it_with_its_pager() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let marker = format!("FPG{}", Uuid::now_v7().simple());
        let ids = listed_items(app, &marker).await;
        let query_id = paged_gather(app, &marker, None).await;
        let path = format!("/gather/{query_id}");
        set_front_page(app, &path).await;

        let page_one = get_front_page(app).await;
        assert_eq!(page_one.status(), StatusCode::OK, "the gather renders at /");
        assert_eq!(location(&page_one), None, "no redirect");
        let front_headers = cache_headers(&page_one);
        let body = body_text(page_one).await;
        assert!(body.contains(&format!("{marker} 1")), "page one:\n{body}");
        assert!(!body.contains(&format!("{marker} 4")), "page one only");
        assert!(
            body.replace("&#x2F;", "/").contains(r#"href="/?page=2""#),
            "the pager stays on the front page:\n{body}"
        );
        assert_eq!(canonical(&body), Some(site_root(app)));

        let page_two = get(app, "/?page=2").await;
        assert_eq!(page_two.status(), StatusCode::OK);
        let body = body_text(page_two).await;
        assert!(body.contains(&format!("{marker} 4")), "page two:\n{body}");
        assert!(!body.contains(&format!("{marker} 1<")), "not page one");

        // The same page at its own address, with the same headers.
        let target = get(app, &format!("{path}?page=2")).await;
        assert_eq!(target.status(), StatusCode::OK);
        assert_eq!(cache_headers(&target), front_headers);
        assert!(body_text(target).await.contains(&format!("{marker} 4")));

        clear_front_page(app).await;
        delete_items(app, &ids).await;
    });
}

/// An alias of a gather is resolved as it would be at its own address.
#[test]
fn front_page_set_to_an_alias_of_a_gather_renders_the_gather() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let marker = format!("FPA{}", Uuid::now_v7().simple());
        let ids = listed_items(app, &marker).await;
        let query_id = paged_gather(app, &marker, None).await;
        let alias = format!("/front-alias-{}", marker.to_lowercase());
        UrlAlias::create(
            &app.db,
            CreateUrlAlias {
                source: format!("/gather/{query_id}"),
                alias: alias.clone(),
                language: Some("en".to_string()),
                stage_id: Some(LIVE_STAGE_ID),
            },
        )
        .await
        .expect("create alias");
        let pool = app.db.clone();
        let cleanup_alias = alias.clone();
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM url_alias WHERE alias = $1")
                .bind(&cleanup_alias)
                .execute(&pool)
                .await;
        });
        set_front_page(app, &alias).await;

        let front = get_front_page(app).await;
        assert_eq!(front.status(), StatusCode::OK, "the alias renders at /");
        assert_eq!(location(&front), None);
        let body = body_text(front).await;
        assert!(body.contains(&format!("{marker} 1")), "the gather:\n{body}");

        let page_two = body_text(get(app, "/?page=2").await).await;
        assert!(
            page_two.contains(&format!("{marker} 4")),
            "page two:\n{page_two}"
        );

        let target = get(app, &alias).await;
        assert_eq!(target.status(), StatusCode::OK, "the alias still works");

        clear_front_page(app).await;
        delete_items(app, &ids).await;
    });
}

/// A plugin's route renders at `/` like any other: the categories plugin's
/// topic browser, which nothing in the front page names.
#[test]
fn front_page_set_to_a_plugin_route_renders_it() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        app.ensure_plugin_enabled("trovato_categories").await;
        set_front_page(app, "/topics").await;

        let target = get(app, "/topics").await;
        assert_eq!(target.status(), StatusCode::OK, "the plugin route serves");
        let target_headers = cache_headers(&target);
        let target_body = body_text(target).await;

        let front = get_front_page(app).await;
        assert_eq!(
            front.status(),
            StatusCode::OK,
            "the plugin route renders at /"
        );
        assert_eq!(location(&front), None);
        assert_eq!(cache_headers(&front), target_headers);
        let body = body_text(front).await;
        assert!(body.contains("Topics"), "the topic browser:\n{body}");
        assert!(
            !body.contains("front-listing") && target_body.contains("Topics"),
            "the plugin page, not the stock front page"
        );
        assert_eq!(canonical(&body), Some(site_root(app)));

        clear_front_page(app).await;
    });
}

/// A visitor who may not see the target is answered at `/` exactly as at the
/// target: here, sent to log in. No row of the listing reaches them, and the
/// stock front page is not served in its place.
#[test]
fn a_target_the_visitor_may_not_see_is_refused_as_at_the_target() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let marker = format!("FPP{}", Uuid::now_v7().simple());
        let ids = listed_items(app, &marker).await;
        let query_id = paged_gather(app, &marker, Some("view front page test listing")).await;
        let path = format!("/gather/{query_id}");
        set_front_page(app, &path).await;

        let target = get(app, &path).await;
        let target_status = target.status();
        let target_location = location(&target);
        assert!(
            target_status == StatusCode::UNAUTHORIZED || target_status == StatusCode::FORBIDDEN,
            "the target refuses anonymous, got {target_status}"
        );

        let front = get_front_page(app).await;
        assert_eq!(front.status(), target_status, "/ answers as the target");
        assert_eq!(location(&front), target_location);
        let body = body_text(front).await;
        assert!(!body.contains(&marker), "no row reaches anonymous:\n{body}");

        clear_front_page(app).await;
        delete_items(app, &ids).await;
    });
}

/// A deleted target is the stock front page, not a 404 or a 500.
#[test]
fn a_deleted_target_falls_back_to_the_stock_front_page() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let title = format!("Deleted Front Page {}", Uuid::now_v7().simple());
        let id = create_item(app, &title, 0, chrono::Utc::now().timestamp()).await;
        set_front_page(app, &format!("/item/{id}")).await;
        assert!(body_text(get_front_page(app).await).await.contains(&title));

        delete_items(app, &[id]).await;
        settle_cache_generations().await;

        let front = get_front_page(app).await;
        assert_eq!(front.status(), StatusCode::OK, "stock front page, not 404");
        assert_eq!(location(&front), None);
        let body = body_text(front).await;
        assert!(!body.contains(&title), "the deleted item is gone");

        // A path no route serves at all is the same.
        set_front_page(app, "/no/such/front/page").await;
        let front = get_front_page(app).await;
        assert_eq!(front.status(), StatusCode::OK);

        clear_front_page(app).await;
    });
}

/// A request other than `GET` reaches the target too: nothing about `/` is
/// special once it has been resolved.
#[test]
fn head_is_answered_as_the_target_answers_it() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        set_front_page(app, "/search").await;

        let front = app
            .request(Request::head("/").body(Body::empty()).unwrap())
            .await;
        let target = app
            .request(Request::head("/search").body(Body::empty()).unwrap())
            .await;
        assert_eq!(front.status(), target.status());
        assert_eq!(cache_headers(&front), cache_headers(&target));

        clear_front_page(app).await;
    });
}

/// POST the site settings form as an administrator, every field at the value
/// it already has except the front page. Returns the status and the body.
///
/// The registration settings a save rewrites are put back as they were found,
/// as `smtp_config_source_test` does, for the other binaries sharing this
/// database.
async fn post_site_form(app: &TestApp, cookies: &str, front_page: &str) -> (StatusCode, String) {
    let mut registration = Vec::new();
    for key in ["user_registration", "allow_user_registration"] {
        registration.push((key, SiteConfig::get(&app.db, key).await.unwrap()));
    }

    let page = app
        .request_with_cookies(
            Request::get("/admin/config/site")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(page.status(), StatusCode::OK);
    let html = body_text(page).await;
    let pipeline = common::form_fields(&html, "site_settings_form");

    let mut site_mail = SiteConfig::site_mail(&app.db).await.unwrap_or_default();
    if site_mail.is_empty() {
        site_mail = "site@front.example.test".to_string();
    }
    let items_per_page = SiteConfig::get(&app.db, "items_per_page")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "10".to_string());
    let mut fields: Vec<(&str, String)> = vec![
        ("site_name", SiteConfig::site_name(&app.db).await.unwrap()),
        (
            "site_slogan",
            SiteConfig::site_slogan(&app.db).await.unwrap_or_default(),
        ),
        ("site_mail", site_mail),
        ("front_page", front_page.to_string()),
        ("items_per_page", items_per_page),
        (
            "registration_mode",
            RegistrationMode::load(&app.db).await.as_str().to_string(),
        ),
    ];
    if SiteConfig::get(&app.db, "notify_admin_on_register")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        fields.push(("notify_admin_on_register", "1".to_string()));
    }
    if trovato_kernel::update_status::setting_allows_check(&app.db).await {
        fields.push(("update_check", "1".to_string()));
    }
    let form = std::iter::once(pipeline)
        .chain(
            fields
                .iter()
                .map(|(k, v)| format!("{k}={}", urlencoding::encode(v))),
        )
        .collect::<Vec<_>>()
        .join("&");

    let response = app
        .request_with_cookies(
            Request::post("/admin/config/site")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let body = body_text(response).await;

    for (key, value) in registration {
        match value {
            Some(value) => SiteConfig::set(&app.db, key, value).await.unwrap(),
            None => {
                SiteConfig::delete(&app.db, key).await.unwrap();
            }
        }
    }
    (status, body)
}

/// The admin form refuses a front page the site answers 404 for, and saves
/// one of any route type that resolves, including one anonymous may not see.
#[test]
fn the_admin_form_saves_only_a_path_that_resolves() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        clear_front_page(app).await;
        let name = format!("frontadm{}", &Uuid::now_v7().simple().to_string()[20..]);
        let cookies = app
            .create_and_login_admin(&name, "Front-Page-Pass-1", &format!("{name}@example.com"))
            .await;

        let (status, body) = post_site_form(app, &cookies, "/no/such/front/page").await;
        assert_eq!(status, StatusCode::OK, "the form is shown again");
        assert!(
            body.contains("does not resolve"),
            "a path nothing serves is refused:\n{body}"
        );
        assert_eq!(
            SiteConfig::front_page(&app.db).await.unwrap(),
            None,
            "nothing was saved"
        );

        for path in ["/search", "/admin/config/site"] {
            let (status, body) = post_site_form(app, &cookies, path).await;
            assert!(
                !body.contains("does not resolve"),
                "{path} resolves ({status}):\n{body}"
            );
            assert_eq!(
                SiteConfig::front_page(&app.db).await.unwrap().as_deref(),
                Some(path),
                "{path} was saved"
            );
        }

        clear_front_page(app).await;
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
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let title = format!("Front Page {marker}");
        let id = create_item(app, &title, 0, chrono::Utc::now().timestamp()).await;

        // The table is the translation plugin's; a fresh database has it only
        // once some test has asked for it.
        common::ensure_translation_table(app);
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
        common::hold_until_cleanup(FRONT_PAGE.lock().await);

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
        common::hold_until_cleanup(FRONT_PAGE.lock().await);

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
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
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

/// A promoted item's teaser carries its body text, under either field name.
///
/// The promoted-items renderer read `body` and nothing else. The kernel `page`
/// type does call the field that, but a content type that declares its own long
/// text field calls it `field_body` — `trovato_blog` does — so a promoted blog
/// post rendered on the front page as a title, a date and a "Read more" link
/// with nothing between them. Both names are pinned here because reading either
/// one alone is the defect (BL-21).
#[test]
fn a_promoted_teaser_shows_its_body_under_either_field_name() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;
        clear_front_page(app).await;

        let base = chrono::Utc::now().timestamp() + 86_400;
        let mut ids = Vec::new();
        let mut expected = Vec::new();

        for (i, field) in ["field_body", "body"].into_iter().enumerate() {
            let tag = Uuid::now_v7().simple().to_string();
            let teaser = format!("The teaser text under {field} {tag}");
            let id = create_item_with_fields(
                app,
                &format!("Promoted Teaser {field} {tag}"),
                1,
                base + i as i64,
                serde_json::json!({ field: { "value": teaser, "format": "plain_text" } }),
            )
            .await;
            ids.push(id);
            expected.push((field, teaser));
        }

        let body = body_text(get_front_page(app).await).await;
        for (field, teaser) in &expected {
            assert!(
                body.contains(teaser.as_str()),
                "a promoted item storing its text as `{field}` must show that text \
                 in its front page teaser: {body}"
            );
        }

        delete_items(app, &ids).await;
    });
}

/// The front page offers its translations, the same as every other item page.
///
/// The item route gave every page `available_translations` and `hreflang_links`;
/// the front route gave the same item neither. So the one page a multilingual
/// site is most likely to be entered on was the one page with no language
/// switcher to leave it by, and the one page a crawler was told nothing about.
///
/// The addresses are the front page's own, `/` and `/it/`, not the item's alias:
/// the reader asked for the front page, and a switcher that lands them on
/// `/it/whatever-the-item-is-called` has moved them somewhere else. The test
/// fetches `/it/` afterwards, because an offered address that does not serve the
/// page is worse than no offer at all.
#[test]
fn a_translated_front_page_offers_its_translations() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let title = format!("Front Switcher {marker}");
        let id = create_item(app, &title, 0, chrono::Utc::now().timestamp()).await;

        // The table is the translation plugin's; a fresh database has it only
        // once some test has asked for it.
        common::ensure_translation_table(app);
        let translated = format!("Selettore {marker}");
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

        let body = body_text(get_front_page(app).await).await;
        let body = body.replace("&#x2F;", "/");
        assert!(
            body.contains(r#"hreflang="it" href="/it/""#),
            "the front page must name its Italian address:\n{body}"
        );
        assert!(
            body.contains(r#"hreflang="x-default" href="/""#),
            "x-default is the front page itself:\n{body}"
        );

        // The offered address has to be a real one.
        let italian = app
            .request(Request::get("/it/").body(Body::empty()).unwrap())
            .await;
        assert_eq!(
            italian.status(),
            StatusCode::OK,
            "the address the switcher offers must serve the page"
        );
        let italian_body = body_text(italian).await;
        assert!(
            italian_body.contains(&translated),
            "and must serve it translated, body was:\n{italian_body}"
        );

        clear_front_page(app).await;
        delete_items(app, &[id]).await;
    });
}

/// A front page with no translation offers no alternates.
///
/// One language is nothing to alternate between, and a self-referential
/// `hreflang` is a tag that says nothing. Same rule the item route follows.
#[test]
fn an_untranslated_front_page_offers_no_alternates() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(FRONT_PAGE.lock().await);
        ensure_item_type(app).await;

        let marker = Uuid::now_v7().simple().to_string();
        let id = create_item(
            app,
            &format!("Front Alone {marker}"),
            0,
            chrono::Utc::now().timestamp(),
        )
        .await;
        set_front_page(app, &format!("/item/{id}")).await;

        let body = body_text(get_front_page(app).await).await;
        assert!(
            !body.contains("hreflang="),
            "one language is nothing to alternate between:\n{body}"
        );

        clear_front_page(app).await;
        delete_items(app, &[id]).await;
    });
}
