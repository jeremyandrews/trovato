#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Private files: a File field whose `settings.private` is `true` marks every
//! file it references as `FileVisibility::Private` (`file_refs.rs`'s sticky
//! promotion), and a private file is never reachable at `/files/{path}` or
//! the image style route — only through `GET /file/{id}/download`, gated the
//! same way the public "any-referencing" policy already gates every file.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::file::FileVisibility;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::CreateItem;
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn small_png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(4, 4, |x, y| image::Rgb([x as u8 * 50, y as u8 * 50, 10]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

async fn create_user(app: &TestApp) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(format!("b291_{}", id.simple()))
    .bind(format!("{}@example.test", id.simple()))
    .execute(&app.db)
    .await
    .expect("seed user");
    id
}

/// A content type with one private File field.
async fn ensure_private_doc_type(app: &TestApp) -> String {
    let name = format!("b291_private_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let mut field = FieldDefinition::new("field_doc", FieldType::File).label("Document");
    field.settings = serde_json::json!({ "private": true });
    let settings = serde_json::json!({ "fields": [field] });
    app.state
        .content_types()
        .replace(&name, "Private Doc", None, settings)
        .await
        .expect("create private doc type");
    app.cleanup_content_type_on_exit(&name);
    let pool = app.db.clone();
    let item_type = name.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE type = $1")
            .bind(&item_type)
            .execute(&pool)
            .await;
    });
    name
}

async fn create_item(
    app: &TestApp,
    item_type: &str,
    author: Uuid,
    status: i16,
    file_id: Uuid,
) -> Uuid {
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: format!("Private doc item {}", Uuid::now_v7().simple()),
                author_id: author,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "field_doc": file_id.to_string() })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &admin(),
        )
        .await
        .expect("create private doc item");
    item.id
}

async fn download(app: &TestApp, id: Uuid) -> axum::response::Response {
    app.request(
        Request::get(format!("/file/{id}/download"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn direct_path_get(app: &TestApp, path: &str) -> axum::response::Response {
    app.request(
        Request::get(format!("/files/{path}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// Attaching a file to a private field marks it private, even though it was
/// a plain (public-scheme) upload — the sticky, fail-closed promotion in
/// `file_refs::sync_item_file_references`.
#[test]
fn attaching_to_a_private_field_marks_the_file_private() {
    run_test(async {
        let app = shared_app().await;
        let type_name = ensure_private_doc_type(app).await;
        let owner = create_user(app).await;

        let upload = app
            .state
            .files()
            .upload(owner, "report.png", "image/png", &small_png())
            .await
            .expect("upload");
        assert_eq!(
            app.state.files().get(upload.id).await.unwrap().unwrap().visibility,
            FileVisibility::Public,
            "a plain upload starts public"
        );

        create_item(app, &type_name, owner, 1, upload.id).await;

        let after = app.state.files().get(upload.id).await.unwrap().unwrap();
        assert_eq!(
            after.visibility,
            FileVisibility::Private,
            "attaching to a private field promotes the file to private"
        );
    });
}

/// A private file on a published item is served by `/file/{id}/download` to
/// the owner, to another viewer, and to an anonymous visitor — the same
/// "any viewer of a published item" policy a public file gets — but is never
/// reachable at the plain `/files/{path}` route, which is public-only.
#[test]
fn private_file_download_route_follows_the_same_view_policy_but_path_route_refuses_it() {
    run_test(async {
        let app = shared_app().await;
        let type_name = ensure_private_doc_type(app).await;
        let owner = create_user(app).await;

        let upload = app
            .state
            .files()
            .upload_private(owner, "secret.png", "image/png", &small_png(), None)
            .await
            .expect("private upload");
        assert_eq!(
            app.state.files().get(upload.id).await.unwrap().unwrap().visibility,
            FileVisibility::Private
        );
        let path = upload
            .uri
            .strip_prefix("local://")
            .expect("local uri")
            .to_string();

        let item_id = create_item(app, &type_name, owner, 1, upload.id).await;

        // Published: owner, any other viewer and anonymous may all download it.
        let resp = download(app, upload.id).await;
        assert_eq!(resp.status(), StatusCode::OK, "published item: downloadable");
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(body.to_vec(), small_png());

        // Never reachable at the plain path, published or not.
        assert_eq!(
            direct_path_get(app, &path).await.status(),
            StatusCode::NOT_FOUND,
            "a private file is never served at the public path route"
        );

        // Unpublished: refused everywhere.
        app.state
            .items()
            .update(
                item_id,
                trovato_kernel::models::UpdateItem {
                    author_id: None,
                    created: None,
                    stage_id: None,
                    title: None,
                    status: Some(0),
                    promote: None,
                    sticky: None,
                    fields: None,
                    log: None,
                },
                &admin(),
            )
            .await
            .unwrap();

        assert_eq!(
            download(app, upload.id).await.status(),
            StatusCode::NOT_FOUND,
            "an unpublished item's private file is refused"
        );
        assert_eq!(
            direct_path_get(app, &path).await.status(),
            StatusCode::NOT_FOUND
        );
    });
}

/// A derivative of a private original is refused at the image style route
/// too — it reuses the same resolved-visibility check as the plain path route.
#[test]
fn image_style_derivative_of_a_private_original_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let type_name = ensure_private_doc_type(app).await;
        let owner = create_user(app).await;

        let upload = app
            .state
            .files()
            .upload_private(owner, "secret.png", "image/png", &small_png(), None)
            .await
            .expect("private upload");
        create_item(app, &type_name, owner, 1, upload.id).await;
        let path = upload.uri.strip_prefix("local://").unwrap();

        let resp = app
            .request(
                Request::get(format!("/files/styles/thumbnail/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    });
}
