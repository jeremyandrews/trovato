#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B160: listing pages and the settings that shape them.
//!
//! - A term page (a gather route whose path names a tag) is titled by the
//!   term, not by the gather's label.
//! - A gather that names no page size of its own pages by the site's items per
//!   page, the setting `/admin/config/site` stored and nothing read.
//! - An item's views are counted when the site counts them, and the count is
//!   shown to a role holding `view post access counter`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use tower::ServiceExt;
use trovato_kernel::gather::types::GatherQuery;
use trovato_kernel::middleware::language::ResolvedLanguage;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{Category, CreateCategory, CreateItem, CreateTag, SiteConfig, Tag};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// The site-setting tests change site-wide settings; one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn stamp() -> String {
    let id = Uuid::now_v7().simple().to_string();
    id[20..].to_string()
}

/// A published conference item, deleted when the test ends.
async fn item(app: &TestApp, title: &str) -> Uuid {
    app.ensure_conference_type().await;
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b160 listing test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;
    let state = app.state.clone();
    defer_cleanup(move || async move {
        state.items().delete(id, &admin()).await.ok();
    });
    id
}

/// Register `query`, removed when the test ends.
async fn register(app: &TestApp, query: &GatherQuery) {
    app.state
        .gather()
        .register_query(query.clone())
        .await
        .expect("register gather");
    let pool = app.db.clone();
    let id = query.query_id.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(&id)
            .execute(&pool)
            .await;
    });
}

/// Set a site setting for the length of the test, restoring what was there.
async fn set_site(app: &TestApp, key: &'static str, value: serde_json::Value) {
    let before = SiteConfig::get(&app.db, key).await.unwrap();
    SiteConfig::set(&app.db, key, value).await.unwrap();
    app.state.gather().clear_site_settings();
    app.state.items().view_counter().clear();
    let state = app.state.clone();
    defer_cleanup(move || async move {
        match before {
            Some(v) => {
                let _ = SiteConfig::set(state.db(), key, v).await;
            }
            None => {
                let _ = sqlx::query("DELETE FROM site_config WHERE key = $1")
                    .bind(key)
                    .execute(state.db())
                    .await;
            }
        }
        state.gather().clear_site_settings();
        state.items().view_counter().clear();
    });
}

async fn body_of(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

/// GET `path` through the display-route router `main` builds at startup.
async fn get_route(app: &TestApp, query: &GatherQuery, path: &str) -> (StatusCode, String) {
    let session_layer = trovato_kernel::session::create_session_layer(
        &trovato_test_utils::env::redis_url(),
        tower_sessions::cookie::SameSite::Strict,
        trovato_kernel::session::CookieSecure::Always,
    )
    .await
    .expect("create session layer");
    let router = trovato_kernel::routes::gather_routes::build_gather_route_router(
        std::slice::from_ref(query),
    )
    .with_state(app.state.clone())
    .layer(axum::Extension(ResolvedLanguage("en".to_string())))
    .layer(session_layer);
    let response = router
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .expect("serve");
    let status = response.status();
    (status, body_of(response).await)
}

fn title_of(html: &str) -> &str {
    let start = html.find("<title>").expect("a <title>") + "<title>".len();
    &html[start..start + html[start..].find("</title>").unwrap()]
}

/// The reproduction: a term page is titled by its term (K-gather-term-title).
#[test]
fn a_term_page_is_titled_by_its_term() {
    run_test(async {
        let app = shared_app().await;
        let s = stamp();
        let category = format!("b160_cat_{s}");
        Category::create(
            &app.db,
            CreateCategory {
                id: category.clone(),
                label: "Recipe category".to_string(),
                description: None,
                hierarchy: Some(0),
                weight: Some(0),
            },
        )
        .await
        .expect("create category");
        let tag = Tag::create(
            &app.db,
            CreateTag {
                category_id: category.clone(),
                label: format!("Desserts {s}"),
                description: None,
                slug: Some(format!("desserts-{s}")),
                weight: Some(0),
                parent_ids: None,
            },
        )
        .await
        .expect("create tag");
        let pool = app.db.clone();
        let (tag_id, cat_id) = (tag.id, category.clone());
        defer_cleanup(move || async move {
            let _ = Tag::delete(&pool, tag_id).await;
            let _ = Category::delete(&pool, &cat_id).await;
        });
        item(app, &format!("TT{s} recipe")).await;

        let route = format!("/b160-recipe-category-{s}/{{slug}}");
        let query: GatherQuery = serde_json::from_value(serde_json::json!({
            "query_id": format!("b160_tt_{s}"),
            "label": "Recipe category",
            "definition": {
                "base_table": "item",
                "item_type": "conference",
                "filters": [{ "field": "title", "operator": "contains", "value": format!("TT{s}") }],
                "sorts": [{ "field": "title", "direction": "asc", "nulls": null }]
            },
            "display": {
                "format": "list",
                "items_per_page": 10,
                "routes": [{
                    "path": route,
                    "params": [{ "segment": "slug", "param": "tag", "tag_category": category }]
                }]
            },
            "plugin": "core"
        }))
        .expect("gather deserializes");
        register(app, &query).await;

        let (status, html) = get_route(
            app,
            &query,
            &format!("/b160-recipe-category-{s}/desserts-{s}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(
            title_of(&html).starts_with(&format!("Desserts {s}")),
            "the page title is the term's name, got {:?}",
            title_of(&html)
        );
        assert!(
            html.contains(&format!(
                "<h2 class=\"gather-query__title\">Desserts {s}</h2>"
            )),
            "the listing heading is the term's name"
        );
    });
}

/// The shipped blog listing's own template heads a tag page with the tag's
/// name, not "Blog" (K-gather-term-title).
#[test]
fn the_blog_tag_page_is_headed_by_its_tag() {
    run_test(async {
        let app = shared_app().await;
        common::ensure_plugin_tables(app, "trovato_blog");
        let (definition, display, label): (serde_json::Value, serde_json::Value, String) =
            sqlx::query_as(
                "SELECT definition, display, label FROM gather_query WHERE query_id = 'blog_listing'",
            )
            .fetch_one(&app.db)
            .await
            .expect("blog_listing is seeded");
        let blog: GatherQuery = serde_json::from_value(serde_json::json!({
            "query_id": "blog_listing",
            "label": label,
            "definition": definition,
            "display": display,
            "plugin": "blog",
        }))
        .expect("blog_listing deserializes");
        // The seeded row, into this server's registry (an upsert of what is there).
        app.state
            .gather()
            .register_query(blog.clone())
            .await
            .expect("register blog_listing");
        let s = stamp();
        let tag = Tag::create(
            &app.db,
            CreateTag {
                category_id: "tags".to_string(),
                label: format!("Pavlova {s}"),
                description: None,
                slug: Some(format!("pavlova-{s}")),
                weight: Some(0),
                parent_ids: None,
            },
        )
        .await
        .expect("create tag");
        let pool = app.db.clone();
        let tag_id = tag.id;
        defer_cleanup(move || async move {
            let _ = Tag::delete(&pool, tag_id).await;
        });

        let (status, html) = get_route(app, &blog, &format!("/blog/tag/pavlova-{s}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(
            html.contains(&format!("<h1>Pavlova {s}</h1>")),
            "the blog tag page is headed by the tag"
        );
        assert!(title_of(&html).starts_with(&format!("Pavlova {s}")));
    });
}

/// A gather with no page size of its own pages by the site's items per page
/// (K-items-per-page); one with its own keeps it.
#[test]
fn a_gather_without_its_own_size_pages_by_the_site_setting() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let app = shared_app().await;
        let s = stamp();
        for n in 1..=4 {
            item(app, &format!("IPP{s}-{n}")).await;
        }
        let gather = |id: &str, own: Option<u32>| -> GatherQuery {
            let mut display = serde_json::json!({ "format": "list" });
            if let Some(own) = own {
                display["items_per_page"] = serde_json::json!(own);
            }
            serde_json::from_value(serde_json::json!({
                "query_id": id,
                "label": "Site page size",
                "definition": {
                    "base_table": "item",
                    "item_type": "conference",
                    "filters": [{ "field": "title", "operator": "contains", "value": format!("IPP{s}") }],
                    "sorts": [{ "field": "title", "direction": "asc", "nulls": null }]
                },
                "display": display,
                "plugin": "core"
            }))
            .unwrap()
        };
        let site_sized = gather(&format!("b160_ipp_site_{s}"), None);
        let own_sized = gather(&format!("b160_ipp_own_{s}"), Some(3));
        assert_eq!(site_sized.display.items_per_page, None);
        register(app, &site_sized).await;
        register(app, &own_sized).await;

        set_site(app, "items_per_page", serde_json::json!("2")).await;

        let count = |html: &str| {
            (1..=4)
                .filter(|n| html.contains(&format!("IPP{s}-{n}<")))
                .count()
        };
        let (status, html) = get(app, &format!("/gather/{}", site_sized.query_id), "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(count(&html), 2, "two to a page: the site's setting");

        let (_, html) = get(app, &format!("/gather/{}", own_sized.query_id), "").await;
        assert_eq!(count(&html), 3, "its own size wins");

        // Saved through the form, the next listing pages by the new value.
        SiteConfig::set(&app.db, "items_per_page", serde_json::json!("4"))
            .await
            .unwrap();
        app.state.gather().clear_site_settings();
        let (_, html) = get(app, &format!("/gather/{}", site_sized.query_id), "").await;
        assert_eq!(count(&html), 4, "the changed setting");
    });
}

/// Views are counted while the site counts them, and the count shown only to a
/// role holding `view post access counter` (K-view-counter).
#[test]
fn item_views_are_counted_and_shown_to_the_permission() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let app = shared_app().await;
        let s = stamp();
        let id = item(app, &format!("Reads {s}")).await;
        let path = format!("/item/{id}");
        let total = || async {
            sqlx::query_scalar::<_, i64>("SELECT total_count FROM item_counter WHERE item_id = $1")
                .bind(id)
                .fetch_optional(&app.db)
                .await
                .unwrap()
        };

        // Off (the default): nothing is counted or shown.
        set_site(app, "count_content_views", serde_json::json!(false)).await;
        let (status, html) = get(app, &path, "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!html.contains("statistics-counter"));
        assert_eq!(total().await, None, "nothing counted while off");

        SiteConfig::set(&app.db, "count_content_views", serde_json::json!(true))
            .await
            .unwrap();
        app.state.items().view_counter().clear();

        let (_, html) = get(app, &path, "").await;
        assert!(
            !html.contains("statistics-counter"),
            "anonymous lacks the permission"
        );
        assert_eq!(total().await, Some(1), "the anonymous view is counted");

        let (_, cookies) = user_holding(app, "b160reads", &["view post access counter"]).await;
        let (_, html) = get(app, &path, &cookies).await;
        assert!(
            html.contains("<span class=\"item__reads statistics-counter\">2 reads</span>"),
            "the holder sees the count, this view included"
        );
        assert_eq!(total().await, Some(2));
    });
}
