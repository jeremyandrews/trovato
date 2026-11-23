//! The JSON API holds the limits and gates the MCP tools hold (B91b).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};
use trovato_kernel::models::{CreateItem, Item};
use uuid::Uuid;

async fn get(app: &common::TestApp, path: &str, cookies: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4_000_000).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// A query longer than the MCP tools accept is refused by the search and the
/// autocomplete too.
#[test]
fn an_overlong_query_is_refused_as_mcp_refuses_it() {
    run_test(async {
        let app = shared_app().await;
        let long = "q".repeat(trovato_kernel::search::MAX_QUERY_LENGTH + 1);
        let (status, body) = get(app, &format!("/api/v1/search?q={long}"), "").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let (status, body) = get(
            app,
            &format!("/api/v1/items/autocomplete?type=page&q={long}"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    });
}

/// A signed-in caller who is not the site administrator lists published items
/// only, whatever `status` they ask for, as the MCP `list_items` tool lists
/// them; the administrator may ask for drafts.
#[test]
fn the_item_list_is_published_only_for_everyone_but_the_administrator() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let (author, cookies) = common::user_holding(app, "b91b_list", &["access content"]).await;
        let draft = Item::create(
            &app.db,
            CreateItem {
                item_type: "conference".to_string(),
                title: format!("b91b draft {}", Uuid::now_v7().simple()),
                author_id: author,
                status: Some(0),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: None,
                language: None,
                log: None,
            },
        )
        .await
        .unwrap();
        let pool = app.db.clone();
        let id = draft.id;
        common::defer_cleanup(move || async move {
            let _ = Item::delete(&pool, id).await;
        });

        let path = format!("/api/v1/items?type=conference&status=0&author_id={author}");
        let (status, body) = get(app, &path, &cookies).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 0, "a draft is not listed: {body}");

        let name = common::username("b91b_list_admin");
        let admin = app
            .create_and_login_admin(&name, "TestPassword123!", &format!("{name}@test.com"))
            .await;
        let (status, body) = get(app, &path, &admin).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1, "the administrator sees the draft: {body}");
    });
}

/// The dead-letter queue answers under `/api/v1` and only to `administer site`.
#[test]
fn the_dead_letter_queue_takes_administer_site() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = common::user_holding(app, "b91b_dlq", &["access content"]).await;
        let (status, _) = get(app, "/api/v1/queue/dlq", &cookies).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, cookies) = common::user_holding(app, "b91b_dlq_site", &["administer site"]).await;
        let (status, body) = get(app, "/api/v1/queue/dlq", &cookies).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["data"].is_array(), "{body}");
    });
}
