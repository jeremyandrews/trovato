#![allow(clippy::unwrap_used, clippy::expect_used)]
//! File fields and the file reference index.
//!
//! A File field stores the bare `file_managed.id` uuid (the upload widget puts
//! `file.id` in its hidden input). The reference index that `/files/{path}`
//! consults used to be built only from `local://` URIs and `/files/` URLs found
//! in strings, so a file attached through a File field had no `file_reference`
//! row: it was treated as an orphan and served only to its uploader and admins,
//! and it was promoted out of `Temporary` only by the two admin form routes, so
//! the six hour temporary file cleanup deleted it under any other write path.
//!
//! These tests pin, at the layers where those defects lived:
//!
//! - a file in a File field (single, and an array for `cardinality: -1`) is
//!   served to an anonymous viewer of a published referencing item and refused
//!   (404) while the item is unpublished;
//! - a file referenced through `ItemService::create` and `ItemService::update`
//!   is `Permanent` afterwards and survives the temporary file cleanup;
//! - a write that adds a reference to a file the acting user may not be served
//!   is refused;
//! - uploads up to `MAX_FILE_SIZE` reach the service, a larger one is a 413
//!   that says why, an extension that disagrees with the content is refused,
//!   and no served response carries `image/svg+xml`.
//!
//! Config import, the third write path, is pinned in
//! `file_reference_import_test.rs` against a scratch database.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, extract_cookies, run_test, shared_app};
use trovato_kernel::file::FileStatus;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, UpdateItem};
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn editor(id: Uuid) -> UserContext {
    UserContext::authenticated(
        id,
        vec![
            "access content".to_string(),
            "create conference content".to_string(),
            "edit own conference content".to_string(),
        ],
    )
}

/// A small but real PNG: 8x8 pixels.
fn small_png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 30, y as u8 * 30, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// A real PNG of noise, sized to land between 2.5 MB and 4 MB: past axum's
/// 2 MB default body limit, well under the 10 MB `MAX_FILE_SIZE`.
fn three_mb_png() -> Vec<u8> {
    // A fixed linear congruential generator: noise does not compress, so the
    // encoded size tracks the pixel count, and the bytes are the same each run.
    let mut seed: u32 = 0x1234_5678;
    let mut next = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 24) as u8
    };
    let img = image::RgbImage::from_fn(1000, 1000, |_, _| image::Rgb([next(), next(), next()]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    let bytes = out.into_inner();
    assert!(
        bytes.len() > 2_500_000 && bytes.len() < 4_000_000,
        "test setup: the PNG should be about 3 MB, got {} bytes",
        bytes.len()
    );
    bytes
}

/// Insert a real non-admin user (`file_managed.owner_id` and `item.author_id`
/// reference `users`).
async fn create_user(app: &TestApp) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(format!("b72_{}", id.simple()))
    .bind(format!("{}@example.test", id.simple()))
    .execute(&app.db)
    .await
    .expect("seed user");
    id
}

/// Upload a small PNG owned by `owner`; return (file id, serve path).
async fn upload_png(app: &TestApp, owner: Uuid) -> (Uuid, String) {
    let filename = format!("logo-{}.png", Uuid::now_v7().simple());
    let up = app
        .state
        .files()
        .upload(owner, &filename, "image/png", &small_png())
        .await
        .expect("upload");
    let path = up.uri.strip_prefix("local://").unwrap().to_string();
    (up.id, path)
}

/// Create a conference as `user` with `fields`.
async fn create_conference(
    app: &TestApp,
    user: &UserContext,
    status: i16,
    fields: serde_json::Value,
) -> anyhow::Result<Uuid> {
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: format!("File field conference {}", Uuid::now_v7().simple()),
                author_id: user.id,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            user,
        )
        .await?;
    Ok(item.id)
}

/// An update that sets only `status` and/or `fields`.
fn update_with(status: Option<i16>, fields: Option<serde_json::Value>) -> UpdateItem {
    UpdateItem {
        author_id: None,
        created: None,
        stage_id: None,
        title: None,
        status,
        promote: None,
        sticky: None,
        fields,
        log: None,
    }
}

/// Anonymous GET of `/files/{path}`.
async fn anon_get(app: &TestApp, path: &str) -> axum::response::Response {
    app.request(
        Request::get(format!("/files/{path}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn file_status(app: &TestApp, id: Uuid) -> Option<FileStatus> {
    app.state.files().get(id).await.unwrap().map(|f| f.status)
}

/// Make the file look as old as the cron cleanup's cutoff, so
/// `cleanup_temp_files(0)` would take it if it were still temporary.
async fn backdate(app: &TestApp, id: Uuid) {
    sqlx::query("UPDATE file_managed SET created = created - 7200 WHERE id = $1")
        .bind(id)
        .execute(&app.db)
        .await
        .unwrap();
}

/// A content type with one File field allowing any number of values.
async fn ensure_gallery_type(app: &TestApp) -> String {
    let name = format!("b72_gallery_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let mut images = FieldDefinition::new("field_images", FieldType::File).label("Images");
    images.cardinality = -1;
    let settings = serde_json::json!({ "fields": [images] });
    app.state
        .content_types()
        .replace(&name, "Gallery", None, settings)
        .await
        .expect("create gallery type");
    app.cleanup_content_type_on_exit(&name);
    // Registered second so it runs first: the type cannot be deleted while
    // items of it exist, and a leaked type grows the shared fixture every pass.
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

/// A single-value File field: the file is served to anonymous viewers of the
/// published item and refused while the item is unpublished.
#[test]
fn single_file_field_is_served_with_its_published_item() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;

        let (file_id, path) = upload_png(app, owner).await;
        let item_id = create_conference(
            app,
            &admin(),
            0,
            serde_json::json!({ "field_logo": file_id.to_string() }),
        )
        .await
        .unwrap();

        assert_eq!(
            anon_get(app, &path).await.status(),
            StatusCode::NOT_FOUND,
            "a file referenced only by an unpublished item is not served to anonymous"
        );

        app.state
            .items()
            .update(item_id, update_with(Some(1), None), &admin())
            .await
            .unwrap();

        let resp = anon_get(app, &path).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a File field's file is served to every viewer of the published item"
        );
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
    });
}

/// A `cardinality: -1` File field holds an array of uuids; every one of them is
/// indexed.
#[test]
fn multi_value_file_field_is_served_with_its_published_item() {
    run_test(async {
        let app = shared_app().await;
        let type_name = ensure_gallery_type(app).await;
        let owner = create_user(app).await;

        let (a, path_a) = upload_png(app, owner).await;
        let (b, path_b) = upload_png(app, owner).await;

        let create = |status: i16| CreateItem {
            item_type: type_name.clone(),
            title: "Gallery".to_string(),
            author_id: Uuid::nil(),
            status: Some(status),
            promote: Some(0),
            sticky: Some(0),
            fields: Some(serde_json::json!({ "field_images": [a.to_string(), b.to_string()] })),
            stage_id: Some(LIVE_STAGE_ID),
            language: Some("en".to_string()),
            log: None,
        };

        let draft = app.state.items().create(create(0), &admin()).await.unwrap();
        for path in [&path_a, &path_b] {
            assert_eq!(
                anon_get(app, path).await.status(),
                StatusCode::NOT_FOUND,
                "unpublished gallery: {path} must not be served to anonymous"
            );
        }

        app.state
            .items()
            .update(draft.id, update_with(Some(1), None), &admin())
            .await
            .unwrap();
        for path in [&path_a, &path_b] {
            assert_eq!(
                anon_get(app, path).await.status(),
                StatusCode::OK,
                "published gallery: {path} is served to anonymous"
            );
        }
    });
}

/// `ItemService::create` promotes the file it references, and the promoted file
/// survives the temporary file cleanup. A backdated unreferenced upload is the
/// control: the cleanup really ran.
#[test]
fn create_promotes_a_referenced_file_and_cleanup_keeps_it() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;

        let (kept, _) = upload_png(app, owner).await;
        let (control, _) = upload_png(app, owner).await;
        assert_eq!(file_status(app, kept).await, Some(FileStatus::Temporary));

        create_conference(
            app,
            &admin(),
            1,
            serde_json::json!({ "field_logo": kept.to_string() }),
        )
        .await
        .unwrap();

        assert_eq!(
            file_status(app, kept).await,
            Some(FileStatus::Permanent),
            "a file an item references is permanent after ItemService::create"
        );

        backdate(app, kept).await;
        backdate(app, control).await;
        app.state.files().cleanup_temp_files(0).await.unwrap();
        assert_eq!(
            file_status(app, control).await,
            None,
            "test control: an unreferenced old temporary file is cleaned up"
        );
        assert_eq!(
            file_status(app, kept).await,
            Some(FileStatus::Permanent),
            "the referenced file survives the cleanup"
        );
    });
}

/// `ItemService::update` promotes a file the update starts referencing.
#[test]
fn update_promotes_a_newly_referenced_file_and_cleanup_keeps_it() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;

        let item_id = create_conference(app, &admin(), 1, serde_json::json!({}))
            .await
            .unwrap();
        let (file_id, path) = upload_png(app, owner).await;

        app.state
            .items()
            .update(
                item_id,
                update_with(
                    None,
                    Some(serde_json::json!({ "field_venue_photo": file_id.to_string() })),
                ),
                &admin(),
            )
            .await
            .unwrap();

        assert_eq!(
            file_status(app, file_id).await,
            Some(FileStatus::Permanent),
            "a file an item references is permanent after ItemService::update"
        );
        backdate(app, file_id).await;
        app.state.files().cleanup_temp_files(0).await.unwrap();
        assert_eq!(file_status(app, file_id).await, Some(FileStatus::Permanent));
        assert_eq!(anon_get(app, &path).await.status(), StatusCode::OK);
    });
}

/// A user may not attach someone else's unreferenced upload: it is a file they
/// cannot be served, and referencing it from a published item would publish it.
#[test]
fn referencing_someone_elses_orphan_upload_is_refused() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let other = create_user(app).await;

        let (theirs, path) = upload_png(app, owner).await;
        let result = create_conference(
            app,
            &editor(other),
            0,
            serde_json::json!({ "field_logo": theirs.to_string() }),
        )
        .await;
        assert!(
            result.is_err(),
            "referencing another user's unreferenced upload must be refused"
        );
        assert_eq!(file_status(app, theirs).await, Some(FileStatus::Temporary));
        assert_eq!(anon_get(app, &path).await.status(), StatusCode::NOT_FOUND);

        // The control: the same user may attach their own upload.
        let (mine, _) = upload_png(app, other).await;
        create_conference(
            app,
            &editor(other),
            0,
            serde_json::json!({ "field_logo": mine.to_string() }),
        )
        .await
        .expect("attaching your own upload is allowed");
        assert_eq!(file_status(app, mine).await, Some(FileStatus::Permanent));
    });
}

/// A file referenced only by an unpublished item the user cannot view is not
/// theirs to attach either.
#[test]
fn referencing_a_file_only_an_unviewable_item_holds_is_refused() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let other = create_user(app).await;

        let (file_id, path) = upload_png(app, owner).await;
        create_conference(
            app,
            &admin(),
            0,
            serde_json::json!({ "field_logo": file_id.to_string() }),
        )
        .await
        .unwrap();

        let result = create_conference(
            app,
            &editor(other),
            0,
            serde_json::json!({ "field_logo": file_id.to_string() }),
        )
        .await;
        assert!(
            result.is_err(),
            "referencing a file held only by an item the user cannot view must be refused"
        );
        assert_eq!(anon_get(app, &path).await.status(), StatusCode::NOT_FOUND);
    });
}

/// A session and a CSRF token for an upload, as the editor's upload widget has.
async fn upload_session(app: &TestApp, prefix: &str) -> (String, String) {
    let name = format!("{prefix}_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let cookies = app
        .create_and_login_admin(&name, "password123", &format!("{name}@example.test"))
        .await;
    let resp = app
        .request_with_cookies(
            Request::get("/admin").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;
    let fresh = extract_cookies(&resp);
    let cookies = if fresh.is_empty() { cookies } else { fresh };
    let html = String::from_utf8(
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let marker = r#"name="_token" value=""#;
    let start = html.find(marker).expect("CSRF token on /admin") + marker.len();
    let end = start + html[start..].find('"').unwrap();
    (cookies, html[start..end].to_string())
}

/// POST `/file/upload` with one file part.
async fn post_upload(
    app: &TestApp,
    session: &(String, String),
    filename: &str,
    content_type: &str,
    data: &[u8],
) -> (StatusCode, serde_json::Value) {
    let boundary = "----b72FileFieldBoundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
         Content-Type: {content_type}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let resp = app
        .request_with_cookies(
            Request::post("/file/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("X-CSRF-Token", &session.1)
                .body(Body::from(body))
                .unwrap(),
            &session.0,
        )
        .await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// A valid 3 MB PNG is under `MAX_FILE_SIZE` (10 MB) and must upload; axum's
/// 2 MB default body limit used to refuse it with a generic 400.
#[test]
fn a_three_megabyte_png_uploads() {
    run_test(async {
        let app = shared_app().await;
        let session = upload_session(app, "b72_up3").await;
        let (status, json) =
            post_upload(app, &session, "photo.png", "image/png", &three_mb_png()).await;
        assert_eq!(status, StatusCode::OK, "3 MB PNG upload: {json}");
        assert_eq!(json["success"], true);
    });
}

/// An 11 MB upload is refused with 413 and a message that says the file is too
/// large, in the upload route's JSON shape.
#[test]
fn an_eleven_megabyte_upload_is_413_with_a_clear_message() {
    run_test(async {
        let app = shared_app().await;
        let session = upload_session(app, "b72_up11").await;
        let mut data = small_png();
        data.resize(11 * 1024 * 1024, 0);
        let (status, json) = post_upload(app, &session, "huge.png", "image/png", &data).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body: {json}");
        assert!(json["code"].is_string(), "an AppError body: {json}");
        let message = json["message"].as_str().unwrap_or("").to_lowercase();
        assert!(
            message.contains("too large"),
            "the error must say the file is too large, got {json}"
        );
    });
}

/// A PNG uploaded as `x.svg` is refused: the name's extension disagrees with
/// the validated content type.
#[test]
fn a_png_named_svg_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let session = upload_session(app, "b72_svg").await;
        let (status, json) = post_upload(app, &session, "x.svg", "image/png", &small_png()).await;
        assert!(
            status.is_client_error(),
            "a PNG named .svg must be refused, got {status}: {json}"
        );
        assert!(json["code"].is_string(), "an AppError body: {json}");
    });
}

/// Serving takes the content type from the stored `filemime`, not the path
/// extension, and never answers `image/svg+xml`: neither for a managed file
/// whose stored name ends in `.svg` nor for an unmanaged `.svg` path.
#[test]
fn no_served_response_carries_the_svg_content_type() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let png = small_png();

        // A managed file stored under an .svg name before upload checked the
        // extension: its stored type is the validated one.
        let id = Uuid::now_v7();
        let uri = format!("local://b72/{}_x.svg", id.simple());
        app.state.files().storage().write(&uri, &png).await.unwrap();
        sqlx::query(
            "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) \
             VALUES ($1, $2, 'x.svg', $3, 'image/png', $4, 0, 0, 0)",
        )
        .bind(id)
        .bind(owner)
        .bind(&uri)
        .bind(png.len() as i64)
        .execute(&app.db)
        .await
        .unwrap();
        create_conference(
            app,
            &admin(),
            1,
            serde_json::json!({ "field_logo": id.to_string() }),
        )
        .await
        .unwrap();

        let resp = anon_get(app, uri.strip_prefix("local://").unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png",
            "the served type is the stored filemime"
        );
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );

        // An unmanaged .svg path: no row to read a type from.
        let unmanaged = format!("local://b72-unmanaged/{}.svg", Uuid::now_v7().simple());
        app.state
            .files()
            .storage()
            .write(&unmanaged, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .await
            .unwrap();
        let resp = anon_get(app, unmanaged.strip_prefix("local://").unwrap()).await;
        let served = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        assert_ne!(
            served, "image/svg+xml",
            "an unmanaged .svg must not be served as SVG"
        );
        app.state.files().storage().delete(&unmanaged).await.ok();
    });
}
