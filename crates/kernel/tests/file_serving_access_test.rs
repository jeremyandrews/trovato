#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FR-8 Story 3.5 — file serving access enforcement (any-referencing, D-29).
//!
//! `serve_uploaded_file` (`GET /files/{path}`) had only a path-traversal guard
//! before streaming bytes. These tests exercise the adopted policy at its own
//! boundary: a file referenced only by a restricted item is denied (404) to a
//! viewer who cannot see that item, an authorized viewer receives the bytes,
//! and a file referenced by no item (orphan / in-flight upload) is servable
//! only to its uploader and admins. They also pin that the `file_reference`
//! index is maintained on item edit (adding/removing a reference flips
//! servability).
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const FILE_BODY: &[u8] = b"top secret attachment contents";

fn admin() -> UserContext {
    UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()])
}

fn stranger() -> UserContext {
    UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()])
}

/// Insert a real (non-admin) user and return its id — a valid `owner_id` for an
/// upload (`file_managed.owner_id` has a FK to `users`).
async fn create_user(app: &common::TestApp) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(format!("owner_{}", id.simple()))
    .bind(format!("{}@example.test", id.simple()))
    .execute(&app.db)
    .await
    .expect("seed owner user");
    id
}

/// Upload a text file owned by `owner`; return (uri, serve-path).
///
/// The filename is made unique per call because the shared test DB is not
/// isolated: two tests uploading the same name would otherwise contend on the
/// `file_managed_uri_key` unique constraint. (Within a single filename the URI
/// is unique regardless — `FileService::upload` embeds the full UUIDv7; the
/// same-millisecond collision that regression is pinned by
/// `concurrent_same_name_uploads_get_distinct_uris`.)
async fn upload(app: &common::TestApp, owner: Uuid) -> (String, String) {
    let filename = format!("note-{}.txt", Uuid::now_v7().simple());
    let up = app
        .state
        .files()
        .upload(owner, &filename, "text/plain", FILE_BODY)
        .await
        .expect("upload");
    let path = up.uri.strip_prefix("local://").unwrap().to_string();
    (up.uri, path)
}

/// Create a conference whose `field_city` value embeds `reference` (a file uri
/// or `/files/` URL), so the item references that file.
async fn item_referencing(
    app: &common::TestApp,
    title: &str,
    status: i16,
    reference: &str,
) -> Uuid {
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
                fields: Some(serde_json::json!({ "field_city": { "value": reference } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("3.5 file ref test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create")
        .id
}

/// Baseline regression (P06): many same-named uploads racing against the real
/// unique constraint must all succeed with distinct URIs.
///
/// The identical filename forces every URI to share its `{YYYY}/{MM}/…_{name}`
/// stem, so the only thing keeping them apart is the embedded UUID. When
/// `FileService::upload` truncated the UUIDv7 to 16 hex chars, a same-instant
/// batch left only the 12-bit `rand_a` field to disambiguate and collided on
/// `file_managed_uri_key` (the flake that forced unique filenames here). The
/// full UUID makes all uploads distinct. Deterministic sibling:
/// `build_storage_uri_embeds_full_uuid_no_same_millisecond_collision`.
#[test]
fn concurrent_same_name_uploads_get_distinct_uris() {
    run_test(async {
        let app = shared_app().await;
        let owner = create_user(app).await;
        // Unique across test runs (shared DB), identical across this batch so the
        // uploads contend on one URI stem.
        let filename = format!("race-{}.txt", Uuid::now_v7().simple());

        let mut handles = Vec::new();
        for _ in 0..16 {
            let files = app.state.files().clone();
            let fname = filename.clone();
            handles.push(tokio::spawn(async move {
                files.upload(owner, &fname, "text/plain", FILE_BODY).await
            }));
        }

        let mut uris = std::collections::HashSet::new();
        for handle in handles {
            let up = handle
                .await
                .expect("task join")
                .expect("concurrent same-name upload must succeed");
            assert!(
                uris.insert(up.uri.clone()),
                "duplicate URI from concurrent same-name uploads: {}",
                up.uri
            );
        }
        assert_eq!(uris.len(), 16, "every upload must have a distinct URI");
    });
}

/// AC-1/AC-2 — a file referenced only by a restricted (unpublished) item is 404
/// for an anonymous caller over HTTP (no existence leak).
#[test]
fn serve_denies_anon_for_referenced_restricted_file() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload(app, owner).await;
        item_referencing(app, "Restricted Attachment", 0, &uri).await;

        let resp = app
            .request(
                Request::get(format!("/files/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not receive a file referenced only by an unpublished item"
        );
    });
}

/// AC-2 — an authorized viewer receives the bytes. The file is referenced by a
/// published item on the live stage, which an anonymous caller may view, so the
/// file streams.
#[test]
fn serve_streams_bytes_for_authorized_viewer() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload(app, owner).await;
        item_referencing(app, "Public Attachment", 1, &uri).await;

        let resp = app
            .request(
                Request::get(format!("/files/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "published-item file is servable"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), FILE_BODY, "the real bytes are streamed");
    });
}

/// A file referenced by no item (orphan / in-flight upload) is servable only to
/// its uploader and admins — not to other non-admins.
#[test]
fn orphan_file_servable_only_to_uploader_and_admin() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, _path) = upload(app, owner).await;

        let owner_ctx = UserContext::authenticated(owner, vec!["access content".to_string()]);
        assert!(
            app.state
                .items()
                .can_serve_file(&uri, &owner_ctx)
                .await
                .unwrap(),
            "uploader may fetch their own orphan file"
        );
        assert!(
            !app.state
                .items()
                .can_serve_file(&uri, &stranger())
                .await
                .unwrap(),
            "another non-admin may not fetch an orphan file"
        );
        assert!(
            app.state
                .items()
                .can_serve_file(&uri, &admin())
                .await
                .unwrap(),
            "admin may fetch any file"
        );
    });
}

/// The `file_reference` index is maintained on item edit: while referenced by a
/// restricted item the uploader cannot serve it (referenced files are governed
/// by referencing-item access, D-29); once the edit removes the reference it
/// becomes an orphan the uploader may serve again.
#[test]
fn reference_index_updates_on_item_edit() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, _path) = upload(app, owner).await;
        let owner_ctx = UserContext::authenticated(owner, vec!["access content".to_string()]);

        // Referenced by an unpublished item: even the uploader is denied
        // (referenced ⇒ governed by referencing-item access, not ownership).
        let item_id = item_referencing(app, "Draft With Attachment", 0, &uri).await;
        assert!(
            !app.state
                .items()
                .can_serve_file(&uri, &owner_ctx)
                .await
                .unwrap(),
            "uploader denied while the file is referenced by a restricted item"
        );
        assert!(
            !app.state
                .items()
                .can_serve_file(&uri, &stranger())
                .await
                .unwrap(),
            "stranger denied a referenced restricted file"
        );

        // Edit the item to drop the reference → the file becomes an orphan.
        app.state
            .items()
            .update(
                item_id,
                trovato_kernel::models::UpdateItem {
                    title: None,
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({ "field_city": { "value": "Barga" } })),
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("update")
            .expect("item exists");

        assert!(
            app.state
                .items()
                .can_serve_file(&uri, &owner_ctx)
                .await
                .unwrap(),
            "uploader may serve the file once it is orphaned by the edit"
        );
        assert!(
            !app.state
                .items()
                .can_serve_file(&uri, &stranger())
                .await
                .unwrap(),
            "stranger still denied the orphan file"
        );
    });
}
