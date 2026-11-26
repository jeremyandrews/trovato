#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B160: paged gather links, old and new.
//!
//! Drupal's pager counted `?page=` from 0: `?page=1` was the second page. A
//! gather that replaces a Drupal listing sets `display.pager.zero_based`, and
//! every paged link the old site published lands on the same items, while a
//! gather that has always counted from 1 keeps the links it published.
//!
//! Also here: a page past the end is a 404, and a gather reached at an alias
//! pages on that alias rather than on `/gather/{id}` (B148).
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use common::{TestApp, defer_cleanup, run_test, shared_app};
use tower::ServiceExt;
use trovato_kernel::gather::types::GatherQuery;
use trovato_kernel::middleware::language::ResolvedLanguage;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, CreateUrlAlias, UrlAlias};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// Five published items titled `{tag}-1` … `{tag}-5`, deleted when the test
/// ends. Two to a page: [1, 2], [3, 4], [5].
async fn five_items(app: &TestApp, tag: &str) {
    app.ensure_conference_type().await;
    for n in 1..=5 {
        let id = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: format!("{tag}-{n}"),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("b160 pager test".to_string()),
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
    }
}

/// A gather over the five items, title ascending, two to a page, with a
/// display route at `route`. Removed when the test ends.
async fn gather(app: &TestApp, tag: &str, zero_based: bool, route: &str) -> GatherQuery {
    let query: GatherQuery = serde_json::from_value(serde_json::json!({
        "query_id": format!("b160_{}", tag.to_lowercase().replace('-', "_")),
        "label": "Pager test",
        "definition": {
            "base_table": "item",
            "item_type": "conference",
            "filters": [{ "field": "title", "operator": "contains", "value": tag }],
            "sorts": [{ "field": "title", "direction": "asc", "nulls": null }]
        },
        "display": {
            "format": "list",
            "items_per_page": 2,
            "pager": { "enabled": true, "zero_based": zero_based },
            "routes": [{ "path": route, "params": [] }]
        },
        "plugin": "core"
    }))
    .expect("gather deserializes");
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
    query
}

/// An alias `alias` for `/gather/{id}`, removed when the test ends.
async fn alias(app: &TestApp, query: &GatherQuery, alias: &str) {
    let created = UrlAlias::create(
        &app.db,
        CreateUrlAlias {
            source: format!("/gather/{}", query.query_id),
            alias: alias.to_string(),
            language: Some("en".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create alias");
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = UrlAlias::delete(&pool, created.id).await;
    });
}

async fn body_of(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn get(app: &TestApp, path: &str) -> (StatusCode, String) {
    let response = app
        .request(Request::get(path).body(Body::empty()).unwrap())
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

/// GET `path` through the display-route router `main` builds at startup (the
/// shared router predates these gathers).
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

/// The item numbers (1–5) a page lists.
fn listed(html: &str, tag: &str) -> Vec<u32> {
    (1..=5)
        .filter(|n| html.contains(&format!("{tag}-{n}<")))
        .collect()
}

/// The `href`s of the page's pager.
fn pager_links(html: &str) -> Vec<String> {
    let Some(start) = html.find("<nav class=\"pager\"") else {
        return Vec::new();
    };
    let nav = &html[start..start + html[start..].find("</nav>").unwrap()];
    nav.match_indices("href=\"")
        .map(|(i, m)| {
            let rest = &nav[i + m.len()..];
            rest[..rest.find('"').unwrap()]
                .replace("&#x2F;", "/")
                .replace("&amp;", "&")
        })
        .collect()
}

/// The reproduction: a Drupal `?page=N` link lands on the items Drupal showed
/// there, and the gather's own pager writes the same Drupal addresses.
#[test]
fn an_old_drupal_page_link_lands_on_the_same_items() {
    run_test(async {
        let app = shared_app().await;
        let stamp = Uuid::now_v7().simple().to_string();
        let tag = format!("PGZ{}", &stamp[20..]);
        five_items(app, &tag).await;
        let route = format!("/pgz-route-{}", &stamp[20..]);
        let query = gather(app, &tag, true, &route).await;
        let path = format!("/pgz-{}", &stamp[20..]);
        alias(app, &query, &path).await;

        // Drupal: no ?page= and ?page=0 are the first page, ?page=1 the second.
        for (page, items) in [
            ("", vec![1, 2]),
            ("?page=0", vec![1, 2]),
            ("?page=1", vec![3, 4]),
            ("?page=2", vec![5]),
        ] {
            let (status, html) = get(app, &format!("{path}{page}")).await;
            assert_eq!(status, StatusCode::OK, "{path}{page}");
            assert_eq!(
                listed(&html, &tag),
                items,
                "{path}{page} lists the items Drupal listed there"
            );
        }

        // The pager on the second page links to Drupal's addresses, on the
        // alias the visitor used (B148): the first page has no ?page=.
        let (_, html) = get(app, &format!("{path}?page=1")).await;
        let links = pager_links(&html);
        assert_eq!(
            links.first().map(String::as_str),
            Some(path.as_str()),
            "previous: {links:?}"
        );
        assert!(
            links.contains(&format!("{path}?page=2")),
            "the third page is ?page=2: {links:?}"
        );
        assert!(
            links.iter().all(|l| !l.contains("/gather/")),
            "no link leaves the alias: {links:?}"
        );

        // Past the end is a 404, not an empty page.
        let (status, _) = get(app, &format!("{path}?page=3")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path}?page=3 is past the end"
        );

        // A display route counts the same way.
        let (status, html) = get_route(app, &query, &format!("{route}?page=1")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed(&html, &tag), vec![3, 4], "{route}?page=1");
        assert!(pager_links(&html).contains(&format!("{route}?page=2")));
    });
}

/// A gather that counts from 1 keeps every link it has published.
#[test]
fn a_gather_counting_from_one_keeps_its_links() {
    run_test(async {
        let app = shared_app().await;
        let stamp = Uuid::now_v7().simple().to_string();
        let tag = format!("PGO{}", &stamp[20..]);
        five_items(app, &tag).await;
        let route = format!("/pgo-route-{}", &stamp[20..]);
        let query = gather(app, &tag, false, &route).await;
        let path = format!("/pgo-{}", &stamp[20..]);
        alias(app, &query, &path).await;

        for (page, items) in [
            ("", vec![1, 2]),
            ("?page=1", vec![1, 2]),
            ("?page=2", vec![3, 4]),
            ("?page=3", vec![5]),
        ] {
            let (status, html) = get(app, &format!("{path}{page}")).await;
            assert_eq!(status, StatusCode::OK, "{path}{page}");
            assert_eq!(listed(&html, &tag), items, "{path}{page}");
        }

        let (_, html) = get(app, &format!("{path}?page=2")).await;
        let links = pager_links(&html);
        assert_eq!(
            links.first().map(String::as_str),
            Some(format!("{path}?page=1").as_str()),
            "previous is ?page=1, as it always was: {links:?}"
        );
        assert!(links.contains(&format!("{path}?page=3")), "{links:?}");

        let (status, _) = get(app, &format!("{path}?page=4")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path}?page=4 is past the end"
        );

        let (status, html) = get_route(app, &query, &format!("{route}?page=2")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed(&html, &tag), vec![3, 4], "{route}?page=2");
    });
}
