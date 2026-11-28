#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B291: `trovato import`'s `file` record (B184) on a site whose files are in
//! an S3 compatible bucket, not a local directory.
//!
//! A migration copies a site's files into the bucket ahead of the importer,
//! exactly as it would copy them into `UPLOADS_DIR` on a site using local
//! storage; the `file` record then registers each one under the id, owner and
//! date it gives. `import/file.rs` reads the bytes through the site's own
//! `FileStorage` (`FileStorage::read`), so this must work with no local
//! directory at all — `FileService::local_dir()` is `None` for an S3 backed
//! site, and the importer must not require it.
//!
//! Needs Postgres, Redis and the S3 test store (`TROVATO_TEST_S3_ENDPOINT`,
//! `TROVATO_TEST_S3_BUCKET`, the store's `AWS_ACCESS_KEY_ID` and
//! `AWS_SECRET_ACCESS_KEY`), always set in CI (real MinIO). Without them, a
//! local run starts an in-process fake S3 server instead of skipping
//! (`fake_s3`, below).

mod common;
#[path = "common/fake_s3.rs"]
mod fake_s3;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test};
use serde_json::json;
use trovato_kernel::file::{FileStorage, S3Config, StorageConfig, build_storage};
use trovato_kernel::import::{ImportFailed, import};
use uuid::Uuid;

const FILE_BODY: &[u8] = b"a file a migration already copied into the bucket";

struct Fixture {
    app: TestApp,
    s3: S3Config,
}

static FIXTURE: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();

fn s3_config() -> S3Config {
    trovato_test_utils::env::load_dotenv();
    match (
        std::env::var("TROVATO_TEST_S3_ENDPOINT").ok(),
        std::env::var("TROVATO_TEST_S3_BUCKET").ok(),
    ) {
        (Some(endpoint), Some(bucket)) => S3Config {
            bucket,
            region: Some("us-east-1".to_string()),
            allow_http: endpoint.starts_with("http://"),
            endpoint: Some(endpoint),
            prefix: Some(format!("import-{}", Uuid::now_v7().simple())),
        },
        _ => {
            let (endpoint, bucket) = fake_s3::ensure("trovato-import-fake");
            S3Config {
                bucket,
                region: Some("us-east-1".to_string()),
                allow_http: true,
                endpoint: Some(endpoint),
                prefix: Some(format!("import-{}", Uuid::now_v7().simple())),
            }
        }
    }
}

fn fixture() -> &'static Fixture {
    FIXTURE.get_or_init(|| {
        let s3 = s3_config();
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_fixture(s3)))
            .join()
            .expect("S3 import fixture app init thread panicked")
    })
}

async fn build_fixture(s3: S3Config) -> Fixture {
    let app = TestApp::with_config(|config| {
        config.file_storage = StorageConfig::S3(s3.clone());
    })
    .await;
    assert_eq!(app.state.files().storage().scheme(), "s3");
    assert!(
        app.state.files().local_dir().is_none(),
        "an S3 backed site has no local files directory"
    );
    Fixture { app, s3 }
}

/// A second, independent client on the same bucket and prefix: the migration
/// tool that copied a file's bytes in, before the importer ever ran.
fn migration_tool(f: &Fixture) -> std::sync::Arc<dyn FileStorage> {
    build_storage(
        &StorageConfig::S3(f.s3.clone()),
        std::path::Path::new("/nonexistent"),
        "/files",
    )
    .unwrap()
}

async fn create_user(app: &TestApp) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(format!("s3importer{}", id.simple()))
    .bind(format!("{}@example.test", id.simple()))
    .execute(&app.db)
    .await
    .expect("seed owner user");
    id
}

fn file_line(
    id: Uuid,
    owner: Uuid,
    path: &str,
    filename: &str,
    mime: &str,
    created: i64,
) -> serde_json::Value {
    json!({
        "type": "file", "id": id, "owner_id": owner, "path": path,
        "filename": filename, "mime_type": mime, "created": created,
    })
}

async fn run_lines(
    app: &TestApp,
    lines: &[serde_json::Value],
) -> anyhow::Result<trovato_kernel::import::ImportReport> {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    import(&app.state, text.as_bytes(), false).await
}

#[derive(Debug, sqlx::FromRow)]
struct StoredFileRow {
    owner_id: Uuid,
    filename: String,
    uri: String,
    filemime: String,
    filesize: i64,
    status: i16,
}

async fn stored_file(app: &TestApp, id: Uuid) -> Option<StoredFileRow> {
    sqlx::query_as(
        "SELECT owner_id, filename, uri, filemime, filesize, status FROM file_managed WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await
    .expect("look up the file")
}

/// A file a migration tool already placed in the bucket, named by a `file`
/// record, registers with the size measured from the real bytes and the
/// declared type, and is servable at its path exactly as an upload is.
#[test]
fn a_file_already_in_the_bucket_registers_and_serves() {
    run_test(async {
        let f = fixture();
        let id = Uuid::now_v7();
        let path = format!("b291-s3-test/{}/report.txt", Uuid::now_v7().simple());
        migration_tool(f)
            .write(&format!("local://{path}"), FILE_BODY)
            .await
            .expect("the migration tool writes the bytes directly to the bucket");

        let name = common::username("s3fileowner");
        f.app
            .create_test_user(&name, "test-password-123", &format!("{name}@example.test"))
            .await;
        let owner = common::user_id_of(&f.app, &name).await;
        let cookies = f.app.login(&name, "test-password-123").await;

        let report = run_lines(
            &f.app,
            &[file_line(
                id,
                owner,
                &path,
                "report.txt",
                "text/plain",
                1_554_112_800,
            )],
        )
        .await
        .expect("import the file record");
        assert_eq!(report.of("file").created, 1, "{report:?}");

        let row = stored_file(&f.app, id).await.expect("the file is stored");
        assert_eq!(row.owner_id, owner);
        assert_eq!(row.filename, "report.txt");
        assert_eq!(row.uri, format!("local://{path}"));
        assert_eq!(row.filemime, "text/plain");
        assert_eq!(
            row.filesize,
            FILE_BODY.len() as i64,
            "size is measured from the bucket, not trusted from the record"
        );
        assert_eq!(row.status, 1, "an imported file is Permanent");

        let resp = f
            .app
            .request_with_cookies(
                Request::get(format!("/files/{path}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the imported file is servable to its owner"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), FILE_BODY, "the real bytes from the bucket are streamed");
    });
}

/// A `file` record naming a key the bucket does not hold is refused, rolling
/// back the whole import.
#[test]
fn a_missing_key_is_refused() {
    run_test(async {
        let f = fixture();
        let owner = create_user(&f.app).await;
        let missing_id = Uuid::now_v7();
        let path = format!("b291-s3-test/{}/ghost.txt", Uuid::now_v7().simple());

        let error = run_lines(
            &f.app,
            &[file_line(missing_id, owner, &path, "ghost.txt", "text/plain", 1)],
        )
        .await
        .expect_err("refused");
        let failed = error.downcast_ref::<ImportFailed>().expect("an ImportFailed");
        assert!(failed.message.contains("not found"), "{failed}");
        assert!(stored_file(&f.app, missing_id).await.is_none());
    });
}

/// A `file` record whose path tries to escape with `..` is refused by shape,
/// the same way on an S3 backed site as on a local directory: this check
/// does not depend on there being a directory to escape.
#[test]
fn a_path_with_dotdot_is_refused_by_shape() {
    run_test(async {
        let f = fixture();
        let owner = create_user(&f.app).await;
        let escapee = Uuid::now_v7();

        let error = run_lines(
            &f.app,
            &[file_line(
                escapee,
                owner,
                "../../etc/passwd",
                "passwd",
                "text/plain",
                1,
            )],
        )
        .await
        .expect_err("refused");
        let failed = error.downcast_ref::<ImportFailed>().expect("an ImportFailed");
        assert!(failed.message.contains("relative path"), "{failed}");
        assert!(stored_file(&f.app, escapee).await.is_none());
    });
}
