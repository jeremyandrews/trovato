#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FR-8 Story 3.3 — HTTP-boundary tests for the REST + comment read-path
//! adoption of the shared access-aware seam.
//!
//! These assert **item-level** enforcement at each surface's own boundary: an
//! unpublished item (and comments on it) is invisible (404) to an anonymous
//! caller and visible to a privileged one. Field-level dropping through the same
//! seam is validated end-to-end via the reference plugin in Story 3.8
//! (`field_access_plugin_test.rs`); here the structural fix (routing REST/comment
//! reads through the seam that closes A1/AC-R1/AC-R4) is exercised over real HTTP.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

async fn make_item(app: &common::TestApp, title: &str, status: i16) -> Uuid {
    let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "field_city": { "value": "Barga" } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("3.3 rest test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id
}

#[test]
fn rest_get_item_hides_unpublished_from_anonymous() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "REST Draft", 0).await;
        let published = make_item(app, "REST Public", 1).await;

        // Anonymous: unpublished item is 404 (item-level access via the seam).
        let resp = app
            .request(
                Request::get(format!("/api/item/{draft}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not see an unpublished item over REST"
        );

        // Anonymous: published item on the live stage is visible.
        let resp = app
            .request(
                Request::get(format!("/api/item/{published}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "anon must see a published item over REST"
        );
    });
}

#[test]
fn ssr_view_item_hides_unpublished_from_anonymous() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "SSR Draft", 0).await;

        // SSR HTML view enforces item-level access (view_item -> load_for_view).
        let resp = app
            .request(
                Request::get(format!("/item/{draft}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not see an unpublished item over SSR"
        );
    });
}

/// A1 / AC-R1 CLOSED-BY proof for the REST **list** endpoints. The audit's A1
/// covers `get_item_api`, `list_items_api` (`/api/items`) and `list_items_by_type`
/// (`/api/items/{type}`); the single-item case is proven above. Story 3.3 routed
/// all three through the shared `filter_page_for_view` seam, so an anonymous list
/// must exclude an unpublished item while including a published one.
#[test]
fn rest_list_endpoints_hide_unpublished_from_anonymous() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let _draft = make_item(app, "REST List Draft Unique", 0).await;
        let published = make_item(app, "REST List Public Unique", 1).await;

        for path in ["/api/items?type=conference", "/api/items/conference"] {
            let resp = app
                .request(Request::get(path).body(Body::empty()).unwrap())
                .await;
            assert_eq!(resp.status(), StatusCode::OK, "list {path} should be 200");
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("REST List Draft Unique"),
                "anon list {path} must not include an unpublished item"
            );
            assert!(
                text.contains(&published.to_string()) || text.contains("REST List Public Unique"),
                "anon list {path} must include the published item"
            );
        }
    });
}

#[test]
fn rest_comments_hidden_for_inaccessible_parent() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "REST Draft Comments", 0).await;

        // Comments on an item the anon caller cannot see return 404 (no existence
        // leak), rather than an empty list.
        let resp = app
            .request(
                Request::get(format!("/api/item/{draft}/comments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not read comments on an inaccessible item"
        );
    });
}
