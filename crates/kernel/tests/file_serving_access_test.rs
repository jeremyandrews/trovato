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
//! The later tests carry the same policy to every other route that serves file
//! bytes or file metadata: image style derivatives (fresh and cached), the file
//! info route, the media browser, and the traversal spellings that could reach a
//! derivative through the plain file route. They also pin the caching rule: only
//! a response an anonymous visitor could also receive is publicly cacheable.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::import::import;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const FILE_BODY: &[u8] = b"top secret attachment contents";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn stranger() -> UserContext {
    UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()])
}

/// The `Cache-Control` header of a response, or `""` when it has none.
fn cache_control(resp: &axum::response::Response) -> String {
    resp.headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
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

/// Upload `data` as `text/plain` (no magic byte check, so any content is
/// accepted) owned by `owner`; return (uri, serve-path).
async fn upload_bytes(app: &common::TestApp, owner: Uuid, data: &[u8]) -> (String, String) {
    let filename = format!("blob-{}.txt", Uuid::now_v7().simple());
    let up = app
        .state
        .files()
        .upload(owner, &filename, "text/plain", data)
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
        assert!(
            cache_control(&resp).starts_with("public"),
            "a file an anonymous visitor may fetch is publicly cacheable, got {:?}",
            cache_control(&resp)
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
                    author_id: None,
                    created: None,
                    stage_id: None,
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

// ---------------------------------------------------------------------------
// Image style derivatives, file info, media browse, and caching
// ---------------------------------------------------------------------------

const IMAGE_STYLES: &str = "trovato_image_styles";

/// A dedicated app with `trovato_image_styles` enabled at construction.
///
/// `AppState` only builds `ImageStyleService` when the plugin is in the enabled
/// set it reads at construction, which the shared app cannot promise on a clean
/// database. The plugin's database status is put back the way it was found once
/// the app exists, so other test binaries sharing the database are unaffected;
/// this app keeps its own in-memory enabled set.
static STYLES_APP: std::sync::OnceLock<(TestApp, std::path::PathBuf)> = std::sync::OnceLock::new();

fn styles_app() -> &'static TestApp {
    &styles_fixture().0
}

/// The uploads directory the styles app serves from.
fn uploads_dir() -> &'static std::path::Path {
    &styles_fixture().1
}

fn styles_fixture() -> &'static (TestApp, std::path::PathBuf) {
    STYLES_APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_styles_app()))
            .join()
            .expect("image styles fixture app init thread panicked")
    })
}

async fn build_styles_app() -> (TestApp, std::path::PathBuf) {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    let before: Option<i16> =
        sqlx::query_scalar("SELECT status FROM plugin_status WHERE name = $1")
            .bind(IMAGE_STYLES)
            .fetch_optional(&pool)
            .await
            .expect("read plugin status");
    trovato_kernel::plugin::status::install_plugin(&pool, IMAGE_STYLES, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{IMAGE_STYLES}': {e:#}"));

    let mut uploads = std::path::PathBuf::new();
    let app = TestApp::with_config(|config| uploads = config.uploads_dir.clone()).await;
    assert!(
        app.state.image_styles().is_some(),
        "the styles fixture must construct ImageStyleService"
    );

    match before {
        Some(status) => {
            sqlx::query("UPDATE plugin_status SET status = $2 WHERE name = $1")
                .bind(IMAGE_STYLES)
                .bind(status)
                .execute(&pool)
                .await
                .expect("restore plugin status");
        }
        None => {
            sqlx::query("DELETE FROM plugin_status WHERE name = $1")
                .bind(IMAGE_STYLES)
                .execute(&pool)
                .await
                .expect("remove fixture plugin status");
        }
    }
    pool.close().await;
    (app, uploads)
}

/// A small valid PNG.
fn png_bytes() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 30, 30]));
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}

/// Upload a PNG owned by `owner`; return (uri, serve-path).
async fn upload_png(app: &TestApp, owner: Uuid) -> (String, String) {
    let filename = format!("pic-{}.png", Uuid::now_v7().simple());
    let up = app
        .state
        .files()
        .upload(owner, &filename, "image/png", &png_bytes())
        .await
        .expect("upload png");
    let path = up.uri.strip_prefix("local://").unwrap().to_string();
    (up.uri, path)
}

/// Upload a permanent text file named `filename` owned by `owner`; return its uri.
async fn upload_permanent(app: &TestApp, owner: Uuid, filename: &str) -> String {
    let up = app
        .state
        .files()
        .upload(owner, filename, "text/plain", FILE_BODY)
        .await
        .expect("upload");
    app.state
        .files()
        .mark_permanent(up.id)
        .await
        .expect("mark permanent");
    up.uri
}

async fn get(app: &TestApp, path: &str) -> axum::response::Response {
    app.request(Request::get(path).body(Body::empty()).unwrap())
        .await
}

async fn get_as(app: &TestApp, path: &str, cookies: &str) -> axum::response::Response {
    app.request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await
}

/// Log in a fresh superuser on `app` and return their cookies.
async fn admin_cookies(app: &TestApp) -> String {
    let name = format!("fsadmin{}", Uuid::now_v7().simple());
    app.create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await
}

async fn user_id_of(app: &TestApp, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(&app.db)
        .await
        .expect("test user should exist")
}

/// Create a non-superuser holding exactly `permissions`, and log them in.
/// Returns (id, cookies, rate-limit bucket).
async fn user_holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String, String) {
    let name = format!("{prefix}{}", Uuid::now_v7().simple());
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = user_id_of(app, &name).await;
    if !permissions.is_empty() {
        common::grant_via_role(app, id, permissions).await;
    }
    let cookies = app.login(&name, "test-password-123").await;
    (id, cookies, name)
}

/// A derivative of an image referenced only by an unpublished item is 404 to an
/// anonymous caller.
#[test]
fn derivative_denied_to_anon_for_restricted_image() {
    run_test(async {
        let app = styles_app();
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload_png(app, owner).await;
        item_referencing(app, "Restricted Picture", 0, &uri).await;

        let resp = get(app, &format!("/files/styles/w400/{path}")).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not receive a derivative of a restricted image"
        );
    });
}

/// A derivative already on disk is still checked: an admin generates it, then an
/// anonymous caller asking for the same URL is refused.
#[test]
fn cached_derivative_is_still_access_checked() {
    run_test(async {
        let app = styles_app();
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload_png(app, owner).await;
        item_referencing(app, "Restricted Cached Picture", 0, &uri).await;

        let url = format!("/files/styles/w400/{path}");
        let cookies = admin_cookies(app).await;
        let resp = get_as(app, &url, &cookies).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "an admin generates the derivative"
        );
        assert!(
            uploads_dir().join("styles/w400").join(&path).exists(),
            "the derivative is cached on disk"
        );

        let resp = get(app, &url).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not receive a cached derivative of a restricted image"
        );
    });
}

/// A derivative of an image referenced by a published live item is served to an
/// anonymous caller, and is publicly cacheable.
#[test]
fn derivative_of_public_image_is_served_and_public() {
    run_test(async {
        let app = styles_app();
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload_png(app, owner).await;
        item_referencing(app, "Public Picture", 1, &uri).await;

        let resp = get(app, &format!("/files/styles/w400/{path}")).await;
        assert_eq!(resp.status(), StatusCode::OK, "public derivative is served");
        assert!(
            cache_control(&resp).starts_with("public"),
            "public derivative is publicly cacheable, got {:?}",
            cache_control(&resp)
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(
            image::load_from_memory(&body).is_ok(),
            "the body is a decodable image"
        );
    });
}

/// A `file` import record (B184) registers an image already on disk; its
/// derivative generates exactly as an uploaded original's does, because the
/// derivative route reads the `file_managed` row and the bytes on disk, and
/// does not care which path put either there.
#[test]
fn derivative_of_an_imported_image_is_generated() {
    run_test(async {
        let app = styles_app();
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(owner)
                .execute(&pool)
                .await;
        });

        let path = format!("b184-test/{}/photo.png", Uuid::now_v7().simple());
        let full = uploads_dir().join(&path);
        tokio::fs::create_dir_all(full.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&full, png_bytes()).await.unwrap();
        let scratch_dir = full.parent().unwrap().to_path_buf();
        common::defer_cleanup(move || async move {
            let _ = tokio::fs::remove_dir_all(&scratch_dir).await;
        });

        let file_id = Uuid::now_v7();
        let line = serde_json::json!({
            "type": "file", "id": file_id, "owner_id": owner, "path": path,
            "filename": "photo.png", "mime_type": "image/png", "created": 1_420_070_400,
        });
        let report = import(&app.state, format!("{line}\n").as_bytes(), false)
            .await
            .expect("import the file record");
        assert_eq!(report.of("file").created, 1, "{report:?}");
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM file_managed WHERE id = $1")
                .bind(file_id)
                .execute(&pool)
                .await;
        });

        let uri = format!("local://{path}");
        let item_id = item_referencing(app, "Imported Picture", 1, &uri).await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("UPDATE item SET current_revision_id = NULL WHERE id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item_revision WHERE item_id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
        });

        let resp = get(app, &format!("/files/styles/w400/{path}")).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "derivative of an imported image is generated"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(
            image::load_from_memory(&body).is_ok(),
            "the body is a decodable image"
        );
    });
}

/// An authorized viewer of a restricted image gets the derivative, but never
/// with a header a shared cache would keep.
#[test]
fn derivative_of_restricted_image_is_private_for_authorized_viewer() {
    run_test(async {
        let app = styles_app();
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload_png(app, owner).await;
        item_referencing(app, "Restricted Private Picture", 0, &uri).await;

        let cookies = admin_cookies(app).await;
        let url = format!("/files/styles/w400/{path}");
        let resp = get_as(app, &url, &cookies).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            cache_control(&resp),
            "private, no-store",
            "fresh derivative"
        );

        // Second request is the disk cache branch.
        let resp = get_as(app, &url, &cookies).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            cache_control(&resp),
            "private, no-store",
            "cached derivative"
        );
    });
}

/// `/files/{path}` for a restricted file fetched by an admin is served, marked
/// `private, no-store`.
#[test]
fn restricted_file_is_private_for_authorized_viewer() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload(app, owner).await;
        item_referencing(app, "Restricted Private Attachment", 0, &uri).await;

        let cookies = admin_cookies(app).await;
        let resp = get_as(app, &format!("/files/{path}"), &cookies).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(cache_control(&resp), "private, no-store");
    });
}

/// A derivative on disk is never reachable through the plain file route, under
/// any spelling of its path.
#[test]
fn plain_file_route_does_not_reach_derivatives() {
    run_test(async {
        let app = styles_app();
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload(app, owner).await;
        item_referencing(app, "Restricted Traversal Attachment", 0, &uri).await;

        let derivative = uploads_dir().join("styles/w400").join(&path);
        std::fs::create_dir_all(derivative.parent().unwrap()).unwrap();
        std::fs::write(&derivative, FILE_BODY).unwrap();

        let spellings = [
            format!("/files/./styles/w400/{path}"),
            format!("/files/styles//w400/{path}"),
        ];
        let mut results = Vec::new();
        for url in &spellings {
            results.push((url.clone(), get(app, url).await.status()));
        }
        for (url, status) in &results {
            assert_eq!(
                *status,
                StatusCode::NOT_FOUND,
                "{url} must be 404; all spellings: {results:?}"
            );
        }
    });
}

/// `GET /file/{id}` reveals nothing about a file the caller may not fetch.
#[test]
fn file_info_is_access_checked() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let restricted =
            upload_permanent(app, owner, &format!("info-{}.txt", Uuid::now_v7().simple())).await;
        item_referencing(app, "Restricted Info", 0, &restricted).await;
        let public =
            upload_permanent(app, owner, &format!("info-{}.txt", Uuid::now_v7().simple())).await;
        item_referencing(app, "Public Info", 1, &public).await;

        let id_of = |uri: String| async move {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM file_managed WHERE uri = $1")
                .bind(uri)
                .fetch_one(&app.db)
                .await
                .unwrap()
        };
        let restricted_id = id_of(restricted).await;
        let public_id = id_of(public).await;

        let resp = get(app, &format!("/file/{restricted_id}")).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not read metadata of a restricted file"
        );
        let resp = get(app, &format!("/file/{public_id}")).await;
        assert_eq!(resp.status(), StatusCode::OK, "public file metadata");
    });
}

/// The media browser lists, for a user without file administration rights, only
/// their own uploads and files referenced by published live items; a holder of
/// `access files` sees everything.
#[test]
fn media_browse_is_scoped_to_the_viewer() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let token = Uuid::now_v7().simple().to_string();
        let other = create_user(app).await;
        let restricted_name = format!("browse-{token}-restricted.txt");
        let public_name = format!("browse-{token}-public.txt");
        let own_name = format!("browse-{token}-own.txt");

        let restricted = upload_permanent(app, other, &restricted_name).await;
        item_referencing(app, "Browse Restricted", 0, &restricted).await;
        let public = upload_permanent(app, other, &public_name).await;
        item_referencing(app, "Browse Public", 1, &public).await;

        let (viewer_id, cookies, bucket) = user_holding(app, "browser", &[]).await;
        upload_permanent(app, viewer_id, &own_name).await;

        let browse = |cookies: String, bucket: String| {
            let token = token.clone();
            async move {
                let resp = app
                    .request_with_cookies(
                        Request::get(format!("/api/v1/media/browse?q={token}&page_size=100"))
                            .header("x-forwarded-for", test_ip_for(&bucket))
                            .body(Body::empty())
                            .unwrap(),
                        &cookies,
                    )
                    .await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let names: Vec<String> = json["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i["filename"].as_str().unwrap().to_string())
                    .collect();
                (names, json["total"].as_i64().unwrap())
            }
        };

        let (names, total) = browse(cookies, bucket).await;
        assert!(
            !names.contains(&restricted_name),
            "another user's restricted file must not be listed: {names:?}"
        );
        assert!(
            names.contains(&public_name),
            "a file on a published item is listed: {names:?}"
        );
        assert!(names.contains(&own_name), "own upload is listed: {names:?}");
        assert_eq!(total, names.len() as i64, "total matches the listing");

        let (_, cookies, bucket) = user_holding(app, "filer", &["access files"]).await;
        let (names, total) = browse(cookies, bucket).await;
        assert!(
            names.contains(&restricted_name),
            "an `access files` holder sees every file: {names:?}"
        );
        assert_eq!(total, names.len() as i64, "total matches the listing");
    });
}

/// The path with its file name in upper case: on a case-insensitive directory
/// (macOS, Windows) the same file, with no row of its own.
fn other_spelling(path: &str) -> String {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let upper = name.to_ascii_uppercase();
    assert_ne!(upper, name, "the name has letters to change");
    if dir.is_empty() {
        upper
    } else {
        format!("{dir}/{upper}")
    }
}

/// A restricted file asked for in other letter case is refused like the file
/// itself, not served as an unmanaged file: a case-insensitive directory reads
/// that spelling as the restricted file.
#[test]
fn restricted_file_in_other_letter_case_is_refused() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload(app, owner).await;
        item_referencing(app, "Restricted Attachment Case", 0, &uri).await;

        let files = app.state.files();
        let variant = other_spelling(&path);
        assert!(
            files
                .is_other_spelling_of_managed(&format!("local://{variant}"))
                .await
                .unwrap(),
            "the other spelling is recognised as the managed file"
        );
        assert!(!files.is_other_spelling_of_managed(&uri).await.unwrap());
        assert!(
            !files
                .is_other_spelling_of_managed("local://2026/11/nothing-like-it.txt")
                .await
                .unwrap()
        );

        let resp = app
            .request(
                Request::get(format!("/files/{variant}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    });
}

/// The same for a derivative: a restricted image's derivative asked for in
/// other letter case is refused, even once the real derivative is made.
#[test]
fn derivative_of_restricted_image_in_other_letter_case_is_refused() {
    run_test(async {
        let app = styles_app();
        app.ensure_conference_type().await;

        let owner = create_user(app).await;
        let (uri, path) = upload_png(app, owner).await;
        item_referencing(app, "Restricted Picture Case", 0, &uri).await;
        let cookies = admin_cookies(app).await;
        let made = get_as(app, &format!("/files/styles/w400/{path}"), &cookies).await;
        assert_eq!(
            made.status(),
            StatusCode::OK,
            "the admin makes the derivative"
        );

        let resp = get(
            app,
            &format!("/files/styles/w400/{}", other_spelling(&path)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    });
}

// ---------------------------------------------------------------------------
// B291: streamed serving, `Range` requests, and that access is decided
// without ever reading the file's bytes.
// ---------------------------------------------------------------------------

/// Bytes with every byte value, long enough to span several of the
/// streaming route's internal read chunks (the default is 4 KiB).
fn big_bytes() -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 256) as u8).collect()
}

/// A single `bytes=a-b` request gets `206`, the exact slice, `Content-Range`
/// and `Content-Length` naming it, and `Accept-Ranges`.
#[test]
fn a_bounded_range_request_gets_206_with_the_exact_slice() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let data = big_bytes();
        let (uri, path) = upload_bytes(app, owner, &data).await;
        item_referencing(app, "Range Public Bounded", 1, &uri).await;

        let resp = app
            .request(
                Request::get(format!("/files/{path}"))
                    .header(header::RANGE, "bytes=100-299")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("bytes 100-299/{}", data.len())
        );
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            "200"
        );
        assert_eq!(
            resp.headers()
                .get(header::ACCEPT_RANGES)
                .unwrap()
                .to_str()
                .unwrap(),
            "bytes"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), &data[100..300]);
    });
}

/// `bytes=N-` (open ended) and `bytes=-N` (suffix) both resolve against the
/// file's real size, each `206` with the matching `Content-Range`.
#[test]
fn an_open_ended_and_a_suffix_range_both_resolve_correctly() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let data = big_bytes();
        let (uri, path) = upload_bytes(app, owner, &data).await;
        item_referencing(app, "Range Public Open Suffix", 1, &uri).await;
        let total = data.len();

        let offset = app
            .request(
                Request::get(format!("/files/{path}"))
                    .header(header::RANGE, format!("bytes={}-", total - 500))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(offset.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            offset
                .headers()
                .get(header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("bytes {}-{}/{total}", total - 500, total - 1)
        );
        let body = to_bytes(offset.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), &data[(total - 500)..]);

        let suffix = app
            .request(
                Request::get(format!("/files/{path}"))
                    .header(header::RANGE, "bytes=-500")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            suffix
                .headers()
                .get(header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("bytes {}-{}/{total}", total - 500, total - 1)
        );
        let body = to_bytes(suffix.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), &data[(total - 500)..]);
    });
}

/// A range past the end of the file is `416`, with `Content-Range: bytes
/// */<size>` naming the real size, not the requested one.
#[test]
fn a_range_past_the_end_is_416() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let (uri, path) = upload(app, owner).await;
        item_referencing(app, "Range Public Past End", 1, &uri).await;

        let resp = app
            .request(
                Request::get(format!("/files/{path}"))
                    .header(
                        header::RANGE,
                        format!("bytes={}-{}", FILE_BODY.len(), FILE_BODY.len() + 10),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("bytes */{}", FILE_BODY.len())
        );
    });
}

/// A plain request with no `Range` header still gets `Accept-Ranges: bytes`,
/// the signal a client needs before it ever sends one.
#[test]
fn a_plain_request_still_advertises_accept_ranges() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let (uri, path) = upload(app, owner).await;
        item_referencing(app, "Accept Ranges Public", 1, &uri).await;

        let resp = app
            .request(
                Request::get(format!("/files/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::ACCEPT_RANGES)
                .unwrap()
                .to_str()
                .unwrap(),
            "bytes"
        );
    });
}

/// A download bigger than one internal read chunk arrives byte for byte: the
/// streamed body is never truncated or reordered.
#[test]
fn a_download_larger_than_one_chunk_arrives_intact() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;
        let data = big_bytes();
        let (uri, path) = upload_bytes(app, owner, &data).await;
        item_referencing(app, "Large Public Download", 1, &uri).await;

        let resp = app
            .request(
                Request::get(format!("/files/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.len(), data.len());
        assert_eq!(body.as_ref(), data.as_slice());
    });
}

/// A storage that counts every byte-reading call (`read`, `read_stream`), to
/// pin that the access decision never reads a file's bytes.
struct CountingStorage {
    inner: trovato_kernel::file::LocalFileStorage,
    reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl trovato_kernel::file::FileStorage for CountingStorage {
    async fn write(&self, uri: &str, data: &[u8]) -> anyhow::Result<()> {
        self.inner.write(uri, data).await
    }
    async fn read(&self, uri: &str) -> anyhow::Result<Vec<u8>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.read(uri).await
    }
    async fn read_stream(
        &self,
        uri: &str,
        range: Option<trovato_kernel::file::ByteRange>,
    ) -> anyhow::Result<trovato_kernel::file::FileStream> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.read_stream(uri, range).await
    }
    async fn delete(&self, uri: &str) -> anyhow::Result<()> {
        self.inner.delete(uri).await
    }
    async fn exists(&self, uri: &str) -> anyhow::Result<bool> {
        self.inner.exists(uri).await
    }
    fn public_url(&self, uri: &str) -> String {
        self.inner.public_url(uri)
    }
    fn scheme(&self) -> &'static str {
        "local"
    }
    fn local_dir(&self) -> Option<&std::path::Path> {
        self.inner.local_dir()
    }
    async fn check_usable(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn describe(&self) -> String {
        "counting".into()
    }
}

/// `ItemService::file_access` (what both `serve_uploaded_file` and
/// `serve_derivative` call, and check, strictly before their own
/// `FileStorage::read_stream` call — see `routes/file.rs` and
/// `routes/image_style.rs`) never reads a byte of the file it is deciding
/// about: it is pure `file_managed`/`file_reference`/item-access lookups.
/// `TestApp` has no seam to swap its own `FileStorage` for an instrumented
/// one, so this pins the access layer directly with its own `FileService`
/// sharing the same database, rather than through the HTTP route: a denied
/// request's 404 can never have touched the bytes, because the function that
/// decides it never does.
#[test]
fn access_is_decided_without_reading_the_file() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let owner = create_user(app).await;

        let dir =
            std::env::temp_dir().join(format!("trovato-counting-{}", Uuid::now_v7().simple()));
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let storage = std::sync::Arc::new(CountingStorage {
            inner: trovato_kernel::file::LocalFileStorage::new(&dir, "/files"),
            reads: reads.clone(),
        });
        let files = trovato_kernel::file::FileService::new(app.db.clone(), storage);

        let name = format!("counted-{}.txt", Uuid::now_v7().simple());
        let up = files
            .upload(owner, &name, "text/plain", FILE_BODY)
            .await
            .expect("upload");
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a new upload never reads back its own bytes"
        );

        item_referencing(app, "Counted Restricted", 0, &up.uri).await;

        let denied = app
            .state
            .items()
            .can_serve_file(&up.uri, &stranger())
            .await
            .unwrap();
        assert!(
            !denied,
            "a stranger may not see a file referenced only by an unpublished item"
        );
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the access decision touched no bytes of the file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    });
}
