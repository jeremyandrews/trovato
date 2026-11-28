#![allow(clippy::unwrap_used, clippy::expect_used)]
//! BG16: a site whose files are in an S3 bucket serves them exactly as a site
//! whose files are in a directory: private files refused without access and
//! served with it, public files with the long `public` cache header a CDN
//! keeps, image derivatives made once and stored in the bucket for every
//! server, and nothing ever readable from the bucket without the server's
//! credentials.
//!
//! The app here is a whole `TestApp` with `FILE_STORAGE=s3` pointed at the
//! test store (MinIO in CI), under a fresh prefix, with image styles enabled.
//! "Another server" is a second, independent client on the same bucket and
//! prefix: what it can read, every pod can.
//!
//! Needs Postgres, Redis and the S3 test store (`TROVATO_TEST_S3_ENDPOINT`,
//! `TROVATO_TEST_S3_BUCKET`, the store's `AWS_ACCESS_KEY_ID` and
//! `AWS_SECRET_ACCESS_KEY`). Without the store it skips locally and fails
//! under CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test};
use trovato_kernel::file::{FileStorage, S3Config, StorageConfig, build_storage};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const IMAGE_STYLES: &str = "trovato_image_styles";
const BODY: &[u8] = b"an attachment kept in the bucket";

struct Fixture {
    app: TestApp,
    s3: S3Config,
    uploads_dir: std::path::PathBuf,
}

static FIXTURE: std::sync::OnceLock<Option<Fixture>> = std::sync::OnceLock::new();

/// The S3 backed app, or `None` (skip) when no test store is configured
/// outside CI.
fn fixture() -> Option<&'static Fixture> {
    FIXTURE
        .get_or_init(|| {
            let s3 = s3_config()?;
            let handle = common::shared_runtime_handle();
            Some(
                std::thread::spawn(move || handle.block_on(build_fixture(s3)))
                    .join()
                    .expect("S3 fixture app init thread panicked"),
            )
        })
        .as_ref()
}

fn s3_config() -> Option<S3Config> {
    trovato_test_utils::env::load_dotenv();
    match (
        std::env::var("TROVATO_TEST_S3_ENDPOINT").ok(),
        std::env::var("TROVATO_TEST_S3_BUCKET").ok(),
    ) {
        (Some(endpoint), Some(bucket)) => Some(S3Config {
            bucket,
            region: Some("us-east-1".to_string()),
            allow_http: endpoint.starts_with("http://"),
            endpoint: Some(endpoint),
            prefix: Some(format!("serving-{}", Uuid::now_v7().simple())),
        }),
        _ => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI must run the S3 serving tests: set TROVATO_TEST_S3_ENDPOINT and \
                 TROVATO_TEST_S3_BUCKET (the workflow starts MinIO for them)"
            );
            eprintln!("skipping: no S3 test store (TROVATO_TEST_S3_ENDPOINT) configured");
            None
        }
    }
}

async fn build_fixture(s3: S3Config) -> Fixture {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    // Image styles enabled for this app only, as `file_serving_access_test`
    // does: install, build, put the shared row back as it was.
    let before: Option<i16> =
        sqlx::query_scalar("SELECT status FROM plugin_status WHERE name = $1")
            .bind(IMAGE_STYLES)
            .fetch_optional(&pool)
            .await
            .expect("read plugin status");
    trovato_kernel::plugin::status::install_plugin(&pool, IMAGE_STYLES, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{IMAGE_STYLES}': {e:#}"));

    let mut uploads_dir = std::path::PathBuf::new();
    let storage = StorageConfig::S3(s3.clone());
    let app = TestApp::with_config(|config| {
        config.file_storage = storage;
        uploads_dir = config.uploads_dir.clone();
    })
    .await;
    assert_eq!(app.state.files().storage().scheme(), "s3");
    assert!(app.state.image_styles().is_some());

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
    Fixture {
        app,
        s3,
        uploads_dir,
    }
}

/// A second client on the same bucket and prefix: another server.
fn other_server(f: &Fixture) -> std::sync::Arc<dyn FileStorage> {
    build_storage(
        &StorageConfig::S3(f.s3.clone()),
        std::path::Path::new("/nonexistent"),
        "/files",
    )
    .unwrap()
}

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

async fn create_user(app: &TestApp) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(format!("s3owner_{}", id.simple()))
    .bind(format!("{}@example.test", id.simple()))
    .execute(&app.db)
    .await
    .expect("seed owner user");
    id
}

/// Upload `data` as `filename`; return (uri, serve path).
async fn upload(
    app: &TestApp,
    owner: Uuid,
    filename: &str,
    mime: &str,
    data: &[u8],
) -> (String, String) {
    let up = app
        .state
        .files()
        .upload(owner, filename, mime, data)
        .await
        .expect("upload");
    let path = up.uri.strip_prefix("local://").unwrap().to_string();
    (up.uri, path)
}

/// A conference item, published (`status` 1) or not (0), referencing `uri`.
async fn item_referencing(app: &TestApp, title: &str, status: i16, uri: &str) {
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
                fields: Some(serde_json::json!({ "field_city": { "value": uri } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("BG16 S3 serving test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create item");
}

async fn get(app: &TestApp, path: &str, cookies: Option<&str>) -> axum::response::Response {
    let request = Request::get(path).body(Body::empty()).unwrap();
    match cookies {
        Some(cookies) => app.request_with_cookies(request, cookies).await,
        None => app.request(request).await,
    }
}

fn header_of(resp: &axum::response::Response, name: header::HeaderName) -> String {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn admin_cookies(app: &TestApp) -> String {
    let name = format!("s3admin{}", Uuid::now_v7().simple());
    app.create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await
}

fn png_bytes() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(16, 16, image::Rgb([20, 120, 200]));
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}

/// An upload lands in the bucket, where another server reads it, and not on
/// this server's disk.
#[test]
fn an_upload_is_stored_in_the_bucket_for_every_server() {
    run_test(async {
        let Some(f) = fixture() else { return };
        let owner = create_user(&f.app).await;
        let name = format!("shared-{}.txt", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "text/plain", BODY).await;
        assert_eq!(other_server(f).read(&uri).await.unwrap(), BODY);
        assert!(
            !f.uploads_dir.join(&path).exists(),
            "the upload must not be written to the local uploads directory"
        );
    });
}

/// A file referenced only by an unpublished item is 404 to an anonymous
/// visitor, and served, uncacheable, to an administrator.
#[test]
fn a_private_file_is_refused_without_access_and_served_with_it() {
    run_test(async {
        let Some(f) = fixture() else { return };
        f.app.ensure_conference_type().await;
        let owner = create_user(&f.app).await;
        let name = format!("private-{}.txt", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "text/plain", BODY).await;
        item_referencing(&f.app, "S3 Private Attachment", 0, &uri).await;

        let resp = get(&f.app, &format!("/files/{path}"), None).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "anonymous is refused");

        let cookies = admin_cookies(&f.app).await;
        let resp = get(&f.app, &format!("/files/{path}"), Some(&cookies)).await;
        assert_eq!(resp.status(), StatusCode::OK, "an administrator is served");
        assert_eq!(header_of(&resp, header::CACHE_CONTROL), "private, no-store");
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], BODY);
    });
}

/// A file a published item references is served to anyone with the headers
/// a CDN keeps for a week, its stored type, and `nosniff`.
#[test]
fn a_public_file_is_served_with_cdn_friendly_headers() {
    run_test(async {
        let Some(f) = fixture() else { return };
        f.app.ensure_conference_type().await;
        let owner = create_user(&f.app).await;
        let name = format!("public-{}.txt", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "text/plain", BODY).await;
        item_referencing(&f.app, "S3 Public Attachment", 1, &uri).await;

        let resp = get(&f.app, &format!("/files/{path}"), None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            header_of(&resp, header::CACHE_CONTROL),
            "public, max-age=604800"
        );
        assert_eq!(header_of(&resp, header::CONTENT_TYPE), "text/plain");
        assert_eq!(
            header_of(&resp, "x-content-type-options".parse().unwrap()),
            "nosniff"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], BODY);
    });
}

/// The object itself cannot be fetched from the store without credentials:
/// the only way to a file is Trovato's route and its access check. This checks
/// the deployment (the store's policy), not Trovato's code: on AWS the same
/// holds by Block Public Access and the bucket policy (part 2).
#[test]
fn the_bucket_does_not_answer_an_unsigned_request() {
    run_test(async {
        let Some(f) = fixture() else { return };
        f.app.ensure_conference_type().await;
        let owner = create_user(&f.app).await;
        let name = format!("unsigned-{}.txt", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "text/plain", BODY).await;
        item_referencing(&f.app, "S3 Unsigned Attachment", 1, &uri).await;

        let endpoint = f.s3.endpoint.as_deref().unwrap().trim_end_matches('/');
        let prefix = f.s3.prefix.as_deref().unwrap();
        let url = format!("{endpoint}/{}/{prefix}/{path}", f.s3.bucket);
        assert!(
            other_server(f).exists(&uri).await.unwrap(),
            "the URL is built from the stored key"
        );
        let resp = reqwest::get(&url).await.expect("reach the store");
        assert_eq!(
            resp.status().as_u16(),
            403,
            "an unsigned GET of a public file's object must be refused"
        );
    });
}

/// An image style derivative is made once and stored in the bucket, where
/// every server finds it; a stored derivative of a restricted image is still
/// refused to an anonymous visitor.
#[test]
fn image_derivatives_are_stored_in_the_bucket_and_still_access_checked() {
    run_test(async {
        let Some(f) = fixture() else { return };
        f.app.ensure_conference_type().await;
        let owner = create_user(&f.app).await;

        let name = format!("pic-{}.png", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "image/png", &png_bytes()).await;
        item_referencing(&f.app, "S3 Public Picture", 1, &uri).await;
        let url = format!("/files/styles/w8/{path}");
        let resp = get(&f.app, &url, None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(header_of(&resp, header::CACHE_CONTROL).starts_with("public"));
        let made = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(image::load_from_memory(&made).is_ok());
        let stored = other_server(f)
            .read(&format!("local://styles/w8/{path}"))
            .await
            .expect("the derivative is in the bucket");
        assert_eq!(stored, made.to_vec());
        assert!(!f.uploads_dir.join("styles/w8").join(&path).exists());

        // Served again from the bucket: replace the stored bytes and see them.
        let marker = png_bytes();
        other_server(f)
            .write(&format!("local://styles/w8/{path}"), &marker)
            .await
            .unwrap();
        let again = get(&f.app, &url, None).await;
        assert_eq!(
            header_of(&again, "x-content-type-options".parse().unwrap()),
            "nosniff",
            "a derivative read back from the bucket carries nosniff"
        );
        let again = to_bytes(again.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            again.to_vec(),
            marker,
            "a stored derivative is served from the bucket"
        );

        let name = format!("secret-{}.png", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "image/png", &png_bytes()).await;
        item_referencing(&f.app, "S3 Restricted Picture", 0, &uri).await;
        let url = format!("/files/styles/w8/{path}");
        let cookies = admin_cookies(&f.app).await;
        assert_eq!(
            get(&f.app, &url, Some(&cookies)).await.status(),
            StatusCode::OK
        );
        assert!(
            other_server(f)
                .exists(&format!("local://styles/w8/{path}"))
                .await
                .unwrap()
        );
        assert_eq!(
            get(&f.app, &url, None).await.status(),
            StatusCode::NOT_FOUND
        );
    });
}

/// A style name is never a path: `%2F` in it, which axum decodes, must not
/// reach a private original's real derivative while the access check runs on
/// a path that names no file. And the original named after the style can
/// never be another derivative, whose bytes would be resized and served.
#[test]
fn a_private_derivative_is_not_reached_by_spelling_tricks() {
    run_test(async {
        let Some(f) = fixture() else { return };
        f.app.ensure_conference_type().await;
        let owner = create_user(&f.app).await;
        let name = format!("hidden-{}.png", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "image/png", &png_bytes()).await;
        item_referencing(&f.app, "S3 Hidden Picture", 0, &uri).await;

        // An administrator makes the real derivative, so it is stored.
        let cookies = admin_cookies(&f.app).await;
        let real = format!("/files/styles/w8/{path}");
        assert_eq!(
            get(&f.app, &real, Some(&cookies)).await.status(),
            StatusCode::OK
        );
        assert!(
            other_server(f)
                .exists(&format!("local://styles/w8/{path}"))
                .await
                .unwrap()
        );

        let (dir, file) = path.rsplit_once('/').unwrap();
        let encoded_dir = dir.replace('/', "%2F");
        // An unmanaged object at the root named like the file: with the style
        // decoded to `w8/<dir>`, the access check would run on this (no row,
        // stored, so allowed) while the read reached the private original's real
        // derivative.
        let decoy = format!("local://{file}");
        other_server(f).write(&decoy, &png_bytes()).await.unwrap();
        for url in [
            format!("/files/styles/w8%2F{encoded_dir}/{file}"),
            format!("/files/styles/w8/styles/w8/{path}"),
            format!("/files/styles/w16/styles/w8/{path}"),
        ] {
            let resp = get(&f.app, &url, None).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{url} must be 404");
        }
        other_server(f).delete(&decoy).await.unwrap();
    });
}

/// Trovato's own working objects (the startup check's sentinel, a local
/// write's temporary file) have no `file_managed` row; they are never served
/// as unmanaged files.
#[test]
fn trovato_working_objects_are_never_served() {
    run_test(async {
        let Some(f) = fixture() else { return };
        let id = Uuid::now_v7().simple();
        for key in [
            format!(".trovato-write-check-{id}"),
            format!("2026/11/.trovato-tmp-{id}"),
        ] {
            other_server(f)
                .write(&format!("local://{key}"), b"working")
                .await
                .unwrap();
            let resp = get(&f.app, &format!("/files/{key}"), None).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{key} must be 404");
            other_server(f)
                .delete(&format!("local://{key}"))
                .await
                .unwrap();
        }
    });
}

/// A deleted image's derivatives stay in the bucket, but are never served:
/// with the original gone there is no row to check access against, so the
/// derivative of a private image would otherwise become public. Derivatives
/// also carry `nosniff`, as originals do.
#[test]
fn a_deleted_images_derivative_is_not_served() {
    run_test(async {
        let Some(f) = fixture() else { return };
        f.app.ensure_conference_type().await;
        let owner = create_user(&f.app).await;
        let name = format!("gone-{}.png", Uuid::now_v7().simple());
        let (uri, path) = upload(&f.app, owner, &name, "image/png", &png_bytes()).await;
        item_referencing(&f.app, "S3 Deleted Picture", 0, &uri).await;

        let cookies = admin_cookies(&f.app).await;
        let url = format!("/files/styles/w8/{path}");
        let made = get(&f.app, &url, Some(&cookies)).await;
        assert_eq!(made.status(), StatusCode::OK);
        assert_eq!(
            header_of(&made, "x-content-type-options".parse().unwrap()),
            "nosniff"
        );
        let derivative = format!("local://styles/w8/{path}");
        assert!(other_server(f).exists(&derivative).await.unwrap());

        let id: Uuid = sqlx::query_scalar("SELECT id FROM file_managed WHERE uri = $1")
            .bind(&uri)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
        assert!(f.app.state.files().delete(id).await.unwrap());
        assert!(!other_server(f).exists(&uri).await.unwrap());
        assert!(
            other_server(f).exists(&derivative).await.unwrap(),
            "the leftover derivative is still stored: the route must refuse it"
        );

        let resp = get(&f.app, &url, None).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anonymous must get 404"
        );
        other_server(f).delete(&derivative).await.unwrap();
    });
}

/// A key the bucket does not hold is 404, not a store failure: not found is
/// recognised from the S3 client's answer.
#[test]
fn a_missing_object_is_404_not_500() {
    run_test(async {
        let Some(f) = fixture() else { return };
        let path = format!("2026/11/{}_missing.txt", Uuid::now_v7().simple());
        let resp = get(&f.app, &format!("/files/{path}"), None).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    });
}

/// A store that holds files but refuses to delete them, as S3 does on a
/// timeout or a missing permission.
struct NoDelete(trovato_kernel::file::LocalFileStorage);

#[async_trait::async_trait]
impl FileStorage for NoDelete {
    async fn write(&self, uri: &str, data: &[u8]) -> anyhow::Result<()> {
        self.0.write(uri, data).await
    }
    async fn read(&self, uri: &str) -> anyhow::Result<Vec<u8>> {
        self.0.read(uri).await
    }
    async fn delete(&self, _uri: &str) -> anyhow::Result<()> {
        anyhow::bail!("access denied")
    }
    async fn exists(&self, uri: &str) -> anyhow::Result<bool> {
        self.0.exists(uri).await
    }
    fn public_url(&self, uri: &str) -> String {
        self.0.public_url(uri)
    }
    fn scheme(&self) -> &'static str {
        "local"
    }
    async fn check_usable(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn describe(&self) -> String {
        "no-delete".into()
    }
}

/// When the bytes cannot be deleted, the row stays: an object left with no row
/// would be served to anyone as an unmanaged file. Needs only Postgres.
#[test]
fn a_file_whose_bytes_cannot_be_deleted_keeps_its_row() {
    run_test(async {
        let app = common::shared_app().await;
        let dir =
            std::env::temp_dir().join(format!("trovato-nodelete-{}", Uuid::now_v7().simple()));
        let files = trovato_kernel::file::FileService::new(
            app.db.clone(),
            std::sync::Arc::new(NoDelete(trovato_kernel::file::LocalFileStorage::new(
                &dir, "/files",
            ))),
        );
        let owner = create_user(app).await;
        let name = format!("kept-{}.txt", Uuid::now_v7().simple());
        let up = files
            .upload(owner, &name, "text/plain", BODY)
            .await
            .unwrap();

        assert!(
            files.delete(up.id).await.is_err(),
            "the failure is reported"
        );
        assert!(files.get(up.id).await.unwrap().is_some(), "the row is kept");

        sqlx::query("UPDATE file_managed SET created = 0 WHERE id = $1")
            .bind(up.id)
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(
            files.cleanup_temp_files(1).await.unwrap(),
            0,
            "a file whose bytes were not deleted is not counted cleaned up"
        );
        assert!(
            files.get(up.id).await.unwrap().is_some(),
            "temp file cleanup keeps the row too"
        );
        sqlx::query("DELETE FROM file_managed WHERE id = $1")
            .bind(up.id)
            .execute(&app.db)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// The status report's "File system" row, from its heading to the row's end.
fn file_system_row(html: &str) -> &str {
    let start = html
        .find("File system")
        .expect("the report has a File system row");
    let rest = &html[start..];
    &rest[..rest.find("</tr>").unwrap_or(rest.len())]
}

async fn status_report(app: &TestApp) -> String {
    let cookies = admin_cookies(app).await;
    let resp = get(app, "/admin/reports/status", Some(&cookies)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    // The template escapes `/`.
    String::from_utf8(body.to_vec())
        .unwrap()
        .replace("&#x2F;", "/")
}

/// The status report checks the bucket for real: a usable one is shown by
/// name and prefix, one this server cannot use is an error, and neither shows
/// the store's secret.
#[test]
fn the_status_report_checks_the_bucket() {
    run_test(async {
        let Some(f) = fixture() else { return };
        let html = status_report(&f.app).await;
        let place = format!(
            "Object storage (s3://{}/{})",
            f.s3.bucket,
            f.s3.prefix.as_deref().unwrap()
        );
        assert!(html.contains(&place), "the report shows {place:?}");
        assert!(
            !html.contains("Not usable ("),
            "a usable bucket is no error"
        );
        assert!(file_system_row(&html).contains("badge--success"), "ok");

        let missing = S3Config {
            bucket: format!("no-such-bucket-{}", Uuid::now_v7().simple()),
            ..f.s3.clone()
        };
        let broken = TestApp::with_config(|config| {
            config.file_storage = StorageConfig::S3(missing.clone());
        })
        .await;
        let html = status_report(&broken).await;
        let place = format!(
            "Not usable (s3://{}/{})",
            missing.bucket,
            missing.prefix.as_deref().unwrap()
        );
        assert!(html.contains(&place), "the report shows {place:?}");
        assert!(
            file_system_row(&html).contains("badge--error"),
            "a bucket this server cannot use is an error, not a warning"
        );
        if let Ok(secret) = std::env::var("AWS_SECRET_ACCESS_KEY") {
            assert!(!html.contains(&secret), "the secret is never shown");
        }
    });
}
