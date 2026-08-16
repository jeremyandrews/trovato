#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for the shared access-aware retrieval seam (FR-8 Story
//! 3.2, design §1): the hydrated-item seam `load_for_view_filtered` and the
//! id-stream seam `filter_page_for_view` on `ItemService`.
//!
//! These exercise the seam's **item-level** access enforcement (the tier that
//! needs no `tap_field_access` implementer): a restricted item is dropped for a
//! viewer who cannot see it and passed through for one who can, rank order and
//! `page_size` respected. The seam's **field-level** dropping is inherently
//! plugin-driven (fail-open with no implementer), so it is validated end-to-end
//! through the real reference plugin in Story 3.8; here, with no field-access
//! plugin, every field of a visible item passes through unchanged (fail-open),
//! which these tests also pin.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Create a conference item and return it. `status` 1 = published, 0 = draft.
async fn make_conference(
    app: &common::TestApp,
    admin: &UserContext,
    title: &str,
    status: i16,
    author_id: Uuid,
) -> trovato_kernel::models::Item {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({
                    "field_city": { "value": "Barga" }
                })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("seam test".to_string()),
            },
            admin,
        )
        .await
        .expect("create should succeed")
}

#[test]
fn load_for_view_filtered_denies_restricted_item() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        // An unpublished item authored by someone else.
        let author = Uuid::nil();
        let item = make_conference(app, &admin, "Draft Conf", 0, author).await;

        // A regular authenticated viewer with no relevant permissions and who is
        // not the author cannot see the unpublished item — the seam returns None.
        let stranger =
            UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()]);
        let denied = app
            .state
            .items()
            .load_for_view_filtered(item.id, &stranger, "view")
            .await
            .expect("query ok");
        assert!(denied.is_none(), "restricted item must be invisible (404)");

        // Admin bypasses item access and sees the item with its fields intact
        // (no field-access plugin ⇒ fail-open ⇒ nothing dropped).
        let visible = app
            .state
            .items()
            .load_for_view_filtered(item.id, &admin, "view")
            .await
            .expect("query ok");
        let visible = visible.expect("admin sees the item");
        assert!(
            visible.fields.get("field_city").is_some(),
            "fail-open keeps the field for admin"
        );
    });
}

#[test]
fn load_for_view_filtered_allows_published_item() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let item = make_conference(app, &admin, "Public Conf", 1, Uuid::nil()).await;

        // A viewer with "access content" sees a published item on the live stage
        // via the kernel's published-view fast-path, fields intact (fail-open).
        let viewer = UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()]);
        let got = app
            .state
            .items()
            .load_for_view_filtered(item.id, &viewer, "view")
            .await
            .expect("query ok")
            .expect("published item is visible");
        assert_eq!(got.id, item.id);
        assert!(got.fields.get("field_city").is_some());
    });
}

#[test]
fn filter_page_for_view_drops_invisible_and_preserves_rank() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let published = make_conference(app, &admin, "Visible Conf", 1, Uuid::nil()).await;
        let draft = make_conference(app, &admin, "Hidden Conf", 0, Uuid::nil()).await;

        // Candidate order: draft first, then published. A viewer with only
        // "access content" cannot see the draft; the seam returns just the
        // published item (rank order among the visible preserved).
        let viewer = UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()]);
        let page = app
            .state
            .items()
            .filter_page_for_view(vec![draft.clone(), published.clone()], &viewer, "view", 10)
            .await;
        assert_eq!(page.len(), 1, "only the published item is visible");
        assert_eq!(page[0].id, published.id);
        assert!(
            page[0].fields.get("field_city").is_some(),
            "fail-open keeps fields on the visible item"
        );

        // Admin sees both, in the given rank order.
        let both = app
            .state
            .items()
            .filter_page_for_view(vec![draft.clone(), published.clone()], &admin, "view", 10)
            .await;
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].id, draft.id, "rank order preserved");
        assert_eq!(both[1].id, published.id);
    });
}

#[test]
fn filter_page_for_view_truncates_to_page_size() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let a = make_conference(app, &admin, "Page A", 1, Uuid::nil()).await;
        let b = make_conference(app, &admin, "Page B", 1, Uuid::nil()).await;
        let c = make_conference(app, &admin, "Page C", 1, Uuid::nil()).await;

        // All visible to admin, but page_size caps the result at 2 (rank order).
        let page = app
            .state
            .items()
            .filter_page_for_view(vec![a.clone(), b.clone(), c.clone()], &admin, "view", 2)
            .await;
        assert_eq!(page.len(), 2, "page truncated to page_size");
        assert_eq!(page[0].id, a.id);
        assert_eq!(page[1].id, b.id);
    });
}
