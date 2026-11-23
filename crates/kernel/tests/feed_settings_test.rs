#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B160: a feed's settings are set through the gather admin form.
//!
//! A feed's number of entries and whether each entry carries the full text
//! (`display.feed.items`, `display.feed.full_content`, B127) are Drupal's "RSS
//! publishing" settings. The gather builder now edits both. And a save through
//! it no longer erases what it does not edit: it posted only the display keys
//! it knew, so saving a gather there dropped its feed and its routes.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, test_ip_for, user_holding};
use serde_json::{Value, json};
use tower::ServiceExt;
use trovato_kernel::gather::types::GatherQuery;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
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

async fn body_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Save the gather through the admin form, posting `display` as the builder
/// would.
async fn save(app: &TestApp, cookies: &str, query: &GatherQuery, display: &Value) -> StatusCode {
    let id = &query.query_id;
    let edit = app
        .request_with_cookies(
            Request::get(format!("/admin/gather/{id}/edit"))
                .header("x-forwarded-for", test_ip_for("b160feed"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let token = token_in(&body_of(edit).await).expect("the form has a token");
    let body = format!(
        "_token={}&_form_build_id=x&query_id={id}&label=Feed%20settings&description=&definition_json={}&display_json={}",
        urlencoding::encode(&token),
        urlencoding::encode(&serde_json::to_string(&query.definition).unwrap()),
        urlencoding::encode(&display.to_string()),
    );
    app.request_with_cookies(
        Request::post(format!("/admin/gather/{id}/save"))
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header("x-forwarded-for", test_ip_for("b160feed"))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// The feed's body, served by the router `main` builds from the gather.
async fn feed(app: &TestApp, query: &GatherQuery) -> String {
    let path = query.display.feed.as_ref().unwrap().path.clone();
    let session_layer = trovato_kernel::session::create_session_layer(
        &trovato_test_utils::env::redis_url(),
        tower_sessions::cookie::SameSite::Strict,
        trovato_kernel::session::CookieSecure::Always,
    )
    .await
    .expect("create session layer");
    let router = trovato_kernel::routes::feed::build_feed_router(std::slice::from_ref(query))
        .with_state(app.state.clone())
        .layer(session_layer);
    let response = router
        .oneshot(Request::get(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_of(response).await
}

#[test]
fn the_admin_form_sets_a_feeds_items_and_full_text_and_keeps_the_rest() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let s = Uuid::now_v7().simple().to_string()[20..].to_string();
        for n in 1..=3 {
            let id = app
                .state
                .items()
                .create(
                    CreateItem {
                        item_type: "conference".to_string(),
                        title: format!("FS{s}-{n}"),
                        author_id: Uuid::nil(),
                        status: Some(1),
                        promote: Some(0),
                        sticky: Some(0),
                        fields: Some(json!({})),
                        stage_id: Some(LIVE_STAGE_ID),
                        language: Some("en".to_string()),
                        log: Some("b160 feed settings".to_string()),
                    },
                    &admin(),
                )
                .await
                .unwrap()
                .id;
            let state = app.state.clone();
            defer_cleanup(move || async move {
                state.items().delete(id, &admin()).await.ok();
            });
        }
        let query: GatherQuery = serde_json::from_value(json!({
            "query_id": format!("b160_feed_{s}"),
            "label": "Feed settings",
            "definition": {
                "base_table": "item",
                "item_type": "conference",
                "filters": [{ "field": "title", "operator": "contains", "value": format!("FS{s}") }],
                "sorts": [{ "field": "title", "direction": "asc", "nulls": null }]
            },
            "display": {
                "format": "list",
                "items_per_page": 10,
                "routes": [{ "path": format!("/b160-feed-{s}"), "params": [] }],
                "feed": { "path": format!("/rss/b160-{s}.xml"), "title": "B160 feed", "items": 20 }
            },
            "plugin": "core"
        }))
        .unwrap();
        app.state
            .gather()
            .register_query(query.clone())
            .await
            .unwrap();
        let state = app.state.clone();
        let id = query.query_id.clone();
        defer_cleanup(move || async move {
            state.gather().delete_query(&id).await.ok();
        });
        assert_eq!(feed(app, &query).await.matches("<item>").count(), 3);

        let (_, cookies) = user_holding(app, "b160feed", &["administer site"]).await;

        // The builder's display, without the keys it does not edit: the stored
        // feed and routes stay.
        let builder_display = json!({
            "format": "list",
            "items_per_page": 10,
            "empty_text": "",
            "canonical_url": null,
            "pager": { "enabled": true, "show_count": false, "style": "full" }
        });
        let status = save(app, &cookies, &query, &builder_display).await;
        assert!(status.is_redirection(), "saved: {status}");
        let saved = app.state.gather().get_query(&query.query_id).unwrap();
        assert_eq!(
            saved.display.feed.as_ref().map(|f| f.path.as_str()),
            Some(format!("/rss/b160-{s}.xml").as_str()),
            "a save that does not mention the feed keeps it"
        );
        assert_eq!(saved.display.routes.len(), 1, "and keeps the routes");

        // The builder's feed controls: two entries, full text.
        let mut with_feed = builder_display.clone();
        let mut feed_settings = serde_json::to_value(saved.display.feed.as_ref().unwrap()).unwrap();
        feed_settings["items"] = json!(2);
        feed_settings["full_content"] = json!(true);
        with_feed["feed"] = feed_settings;
        let status = save(app, &cookies, &query, &with_feed).await;
        assert!(status.is_redirection(), "saved: {status}");
        let saved = app.state.gather().get_query(&query.query_id).unwrap();
        let settings = saved.display.feed.as_ref().unwrap();
        assert_eq!(settings.items, 2);
        assert!(settings.full_content);
        assert_eq!(settings.title.as_deref(), Some("B160 feed"));

        let xml = feed(app, &saved).await;
        assert_eq!(
            xml.matches("<item>").count(),
            2,
            "the feed ends at its second entry"
        );
    });
}
