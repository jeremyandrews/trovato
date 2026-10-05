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
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
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

// =============================================================================
// The read paths that loaded an item and never asked the seam about it.
//
// Every test below pairs a negative with a positive: the fix has to hide what
// the viewer may not see *and* still show what they may, or it could pass by
// returning nothing at all.
// =============================================================================

async fn get_as(
    app: &common::TestApp,
    path: &str,
    cookies: &str,
    bucket: &str,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", common::test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Problem 1 — the revision list required a login and nothing else, so the
/// titles and log messages of every draft were readable by anyone with an
/// account. It is now the same bar the revert button on the same page applies.
#[test]
fn the_revision_list_is_held_to_view_then_edit() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "Revision Seam Draft", 0).await;

        // A logged-in user who cannot view the draft is told it does not exist.
        let (_, nosy) = common::user_holding(app, "revseam-nosy", &[]).await;
        let (status, html) = get_as(
            app,
            &format!("/item/{draft}/revisions"),
            &nosy,
            "revseam-nosy",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a user who cannot view the draft must not read its revision history, got {html}"
        );

        // A user who may view it but not edit it is refused, as the revert
        // button on the same page already refuses them.
        let (_, reader) = common::user_holding(app, "revseam-reader", &["view any content"]).await;
        let (status, html) = get_as(
            app,
            &format!("/item/{draft}/revisions"),
            &reader,
            "revseam-reader",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "reading the revision history is an editor's view, not a reader's, got {html}"
        );

        // And an editor still gets the page.
        let (_, editor) = common::user_holding(
            app,
            "revseam-editor",
            &["view any content", "edit any content"],
        )
        .await;
        let (status, html) = get_as(
            app,
            &format!("/item/{draft}/revisions"),
            &editor,
            "revseam-editor",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an editor must still see the history, got {html}"
        );
        assert!(
            html.contains("Revision Seam Draft"),
            "the editor's page must name the item, got {html}"
        );
    });
}

/// Problem 6 — `create_comment_inner` loaded the item to notify its author and
/// never asked whether the commenter may see it, so `post comments` was enough
/// to comment on (and learn the existence of) a draft.
#[test]
fn a_comment_cannot_be_posted_on_an_item_the_author_cannot_see() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        app.ensure_plugin_enabled("trovato_comments").await;

        let draft = make_item(app, "Comment Seam Draft", 0).await;
        let published = make_item(app, "Comment Seam Public", 1).await;

        let (_, cookies) = common::user_holding(app, "cmtseam", &["post comments"]).await;

        // The draft answers exactly as a missing item does.
        let (status, body) = post_comment_to(app, &cookies, draft).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "commenting on an unviewable item must answer as for a missing one, got {body}"
        );
        assert!(
            body.contains("Item not found"),
            "the refusal must not distinguish denied from missing, got {body}"
        );

        // The same user may still comment on the published item.
        let (status, body) = post_comment_to(app, &cookies, published).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the same commenter must still reach a published item, got {body}"
        );
    });
}

/// POST a comment, returning the status and body.
async fn post_comment_to(
    app: &common::TestApp,
    cookies: &str,
    item_id: Uuid,
) -> (StatusCode, String) {
    // A CSRF token, read off a rendered page in this session.
    let response = app
        .request_with_cookies(
            Request::get("/")
                .header("x-forwarded-for", common::test_ip_for("cmtseam"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let fresh = common::extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let token = html[start..].split('"').next().expect("token").to_string();

    let response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &token)
                .header("x-forwarded-for", common::test_ip_for("cmtseam"))
                .body(Body::from(
                    serde_json::json!({ "body": "Is this item even here?" }).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}
