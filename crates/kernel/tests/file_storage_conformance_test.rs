#![allow(clippy::unwrap_used, clippy::expect_used)]
//! BG16: one `FileStorage` test suite, run against every backend.
//!
//! [`conformance`] is the contract: what any backend must do for the file
//! service, the serving routes and the image style route to work on it. It runs
//! against `LocalFileStorage` on a fresh directory and against `S3FileStorage`
//! on an S3 compatible store (MinIO in CI, started by the workflow). The same
//! file holds the tests of `trovato files migrate-to-s3`'s copy, from a local
//! directory into each backend.
//!
//! The S3 tests need `TROVATO_TEST_S3_ENDPOINT` and `TROVATO_TEST_S3_BUCKET`
//! (an existing bucket) and the store's credentials in `AWS_ACCESS_KEY_ID` and
//! `AWS_SECRET_ACCESS_KEY`. Without them they skip locally and fail under CI
//! (`CI` set), so CI can never pass them by not running them. Each test works
//! under its own random prefix, so runs never see each other's objects.
//!
//! Needs no database.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trovato_kernel::file::migrate::copy_dir_to_storage;
use trovato_kernel::file::{
    FileStorage, LocalFileStorage, S3Config, S3FileStorage, StorageConfig, StorageNotFound,
    build_storage, is_not_found, is_unreachable,
};
use uuid::Uuid;

/// A fresh directory under the system temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("trovato-{tag}-{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The S3 test store's settings with a fresh prefix, or `None` (skip) when no
/// store is configured outside CI.
fn s3_config() -> Option<S3Config> {
    trovato_test_utils::env::load_dotenv();
    let endpoint = std::env::var("TROVATO_TEST_S3_ENDPOINT").ok();
    let bucket = std::env::var("TROVATO_TEST_S3_BUCKET").ok();
    match (endpoint, bucket) {
        (Some(endpoint), Some(bucket)) => Some(S3Config {
            bucket,
            region: Some("us-east-1".to_string()),
            allow_http: endpoint.starts_with("http://"),
            endpoint: Some(endpoint),
            prefix: Some(format!("conformance-{}", Uuid::now_v7().simple())),
        }),
        _ => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI must run the S3 storage tests: set TROVATO_TEST_S3_ENDPOINT and \
                 TROVATO_TEST_S3_BUCKET (the workflow starts MinIO for them)"
            );
            eprintln!("skipping: no S3 test store (TROVATO_TEST_S3_ENDPOINT) configured");
            None
        }
    }
}

fn s3_storage(config: &S3Config) -> Arc<dyn FileStorage> {
    build_storage(
        &StorageConfig::S3(config.clone()),
        Path::new("/nonexistent"),
        "/files",
    )
    .expect("build S3 storage")
}

/// Bytes with every byte value, NUL included, longer than one read buffer.
fn awkward_bytes() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 256) as u8).collect()
}

/// The contract every backend keeps.
async fn conformance(storage: &dyn FileStorage) {
    let key = format!("2026/11/{}_photo-1.jpg", Uuid::now_v7().simple());
    let uri = format!("local://{key}");

    // Nothing there yet: not found, not a store failure.
    assert!(!storage.exists(&uri).await.unwrap());
    let missing = storage.read(&uri).await.expect_err("nothing stored yet");
    assert!(
        is_not_found(&missing),
        "a missing file is not found: {missing:#}"
    );
    assert!(
        missing.downcast_ref::<StorageNotFound>().is_some(),
        "not found is a type: {missing:#}"
    );

    // A write is read back byte for byte, nested directories and all.
    let data = awkward_bytes();
    storage.write(&uri, &data).await.unwrap();
    assert!(storage.exists(&uri).await.unwrap());
    assert_eq!(storage.read(&uri).await.unwrap(), data);

    // An empty file is a file.
    let empty = format!("local://empty/{}.txt", Uuid::now_v7().simple());
    storage.write(&empty, b"").await.unwrap();
    assert!(storage.exists(&empty).await.unwrap());
    assert_eq!(storage.read(&empty).await.unwrap(), b"");

    // A second write replaces the first, shorter or not.
    storage.write(&uri, b"replaced").await.unwrap();
    assert_eq!(storage.read(&uri).await.unwrap(), b"replaced");

    // The URL is Trovato's own route, never one into the store.
    assert_eq!(storage.public_url(&uri), format!("/files/{key}"));

    // Delete removes it; deleting again succeeds.
    storage.delete(&uri).await.unwrap();
    assert!(!storage.exists(&uri).await.unwrap());
    assert!(is_not_found(&storage.read(&uri).await.unwrap_err()));
    storage.delete(&uri).await.unwrap();
    storage.delete(&empty).await.unwrap();

    // No URI can name anything outside the store.
    for bad in [
        "local://../escape.txt",
        "local://a/../../escape.txt",
        "local:///etc/passwd",
        "local://a//b.txt",
        "local://./a.txt",
        "local://a\\..\\b.txt",
        "local://",
        "s3://bucket/a.txt",
        "/etc/passwd",
    ] {
        assert!(storage.write(bad, b"x").await.is_err(), "write {bad:?}");
        let read = storage.read(bad).await;
        assert!(read.is_err(), "read {bad:?}");
        assert!(
            !is_not_found(&read.unwrap_err()),
            "{bad:?} is refused, not missing"
        );
        assert!(storage.exists(bad).await.is_err(), "exists {bad:?}");
        assert!(storage.delete(bad).await.is_err(), "delete {bad:?}");
    }

    // The startup check passes on a usable store.
    storage.check_usable().await.expect("a usable store passes");
}

#[tokio::test]
async fn local_storage_keeps_the_contract() {
    let dir = TempDir::new("conformance-local");
    let storage = LocalFileStorage::new(&dir.0, "/files");
    conformance(&storage).await;
    assert_eq!(storage.scheme(), "local");
    // Nothing is left behind: not the check's sentinel, not a temporary file.
    let mut left = Vec::new();
    let mut dirs = vec![dir.0.clone()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                dirs.push(entry.path());
            } else {
                left.push(entry.path());
            }
        }
    }
    assert!(left.is_empty(), "files left behind: {left:?}");
}

/// The longest name an upload can be stored under (a 32-character id, `_`
/// and a cleaned name of up to 200 bytes) is still written: the temporary
/// name beside it must not push past the file system's 255-byte limit.
#[tokio::test]
async fn local_storage_writes_the_longest_stored_name() {
    let dir = TempDir::new("conformance-long");
    let storage = LocalFileStorage::new(&dir.0, "/files");
    let name = format!("{}_{}.jpg", Uuid::now_v7().simple(), "n".repeat(196));
    assert_eq!(name.len(), 233);
    let uri = format!("local://2026/11/{name}");
    storage.write(&uri, b"long").await.expect("write");
    assert_eq!(storage.read(&uri).await.unwrap(), b"long");
}

#[tokio::test]
async fn s3_storage_keeps_the_contract() {
    let Some(config) = s3_config() else { return };
    let storage = s3_storage(&config);
    conformance(storage.as_ref()).await;
    assert_eq!(storage.scheme(), "s3");
    assert!(storage.local_dir().is_none());
    assert_eq!(
        storage.describe(),
        format!(
            "s3://{}/{}",
            config.bucket,
            config.prefix.as_deref().unwrap()
        )
    );
}

/// The prefix keeps two sites sharing a bucket apart: neither sees the other's
/// file under the same URI.
#[tokio::test]
async fn s3_prefixes_keep_sites_apart() {
    let Some(a) = s3_config() else { return };
    let mut b = a.clone();
    b.prefix = Some(format!("conformance-{}", Uuid::now_v7().simple()));
    let (a, b) = (s3_storage(&a), s3_storage(&b));
    let uri = "local://shared-name.txt";
    a.write(uri, b"site a").await.unwrap();
    assert!(!b.exists(uri).await.unwrap());
    b.write(uri, b"site b").await.unwrap();
    assert_eq!(a.read(uri).await.unwrap(), b"site a");
    assert_eq!(b.read(uri).await.unwrap(), b"site b");
    a.delete(uri).await.unwrap();
    b.delete(uri).await.unwrap();
}

/// A store this server cannot use fails the startup check, naming the bucket
/// and never the credentials.
#[tokio::test]
async fn s3_check_refuses_a_missing_bucket_by_name() {
    let Some(mut config) = s3_config() else {
        return;
    };
    config.bucket = format!("no-such-bucket-{}", Uuid::now_v7().simple());
    let storage = s3_storage(&config);
    let err = storage.check_usable().await.expect_err("must refuse");
    // The store answered: a definite refusal, which stops `serve`, never an
    // outage it starts through.
    assert!(!is_unreachable(&err), "{err:#}");
    let err = format!("{err:#}");
    assert!(err.contains(&config.bucket), "{err}");
    assert!(err.contains("S3_BUCKET"), "{err}");
    if let Ok(secret) = std::env::var("AWS_SECRET_ACCESS_KEY") {
        assert!(
            !err.contains(&secret),
            "the error must not carry the secret"
        );
        let debug = format!("{:?}", S3FileStorage::new(&config, "/files").unwrap());
        assert!(!debug.contains(&secret), "{debug}");
    }
    let debug = format!("{:?}", S3FileStorage::new(&config, "/files").unwrap());
    assert!(!debug.contains("secret"), "{debug}");
}

/// A stand-in S3 endpoint that answers every request with `status` and an
/// empty body. Returns its `http://127.0.0.1:<port>` address.
async fn answering(status: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Read the request head and whatever body came with it, then
                // answer and close.
                let mut buf = vec![0u8; 64 * 1024];
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    socket.read(&mut buf),
                )
                .await;
                let reply =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// The startup check tells a store that is down from one that answered: a
/// server error is an outage the server starts through, while a region's bare
/// redirect or a 400 (what a wrong `S3_REGION` or `S3_ENDPOINT` gets) is a
/// setting to fix and stops it. Needs only the test store's credentials, so
/// the client signs without looking for an instance role.
#[tokio::test]
async fn s3_check_tells_an_outage_from_a_wrong_setting() {
    let Some(base) = s3_config() else { return };
    for (status, outage) in [
        ("503 Service Unavailable", true),
        ("500 Internal Server Error", true),
        ("429 Too Many Requests", true),
        ("301 Moved Permanently", false),
        ("400 Bad Request", false),
    ] {
        let config = S3Config {
            endpoint: Some(answering(status).await),
            allow_http: true,
            ..base.clone()
        };
        let err = s3_storage(&config)
            .check_usable()
            .await
            .expect_err("a store that never succeeds is not usable");
        assert_eq!(is_unreachable(&err), outage, "{status}: {err:#}");
        assert!(format!("{err:#}").contains(&config.bucket), "{status}");
    }
}

/// A store that keeps other bytes than it was given: every copy must fail its
/// read-back checksum and none be counted copied.
struct Corrupting(LocalFileStorage);

#[async_trait::async_trait]
impl FileStorage for Corrupting {
    async fn write(&self, uri: &str, data: &[u8]) -> anyhow::Result<()> {
        let mut bad = data.to_vec();
        bad.push(0);
        self.0.write(uri, &bad).await
    }
    async fn read(&self, uri: &str) -> anyhow::Result<Vec<u8>> {
        self.0.read(uri).await
    }
    async fn delete(&self, uri: &str) -> anyhow::Result<()> {
        self.0.delete(uri).await
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
        "corrupting".into()
    }
}

#[tokio::test]
async fn migration_reports_a_copy_whose_read_back_differs() {
    let src = TempDir::new("migrate-corrupt");
    let outside = TempDir::new("migrate-outside");
    let dest = TempDir::new("migrate-dest");
    let files = uploads_tree(&src.0, &outside.0.join("secret.txt"));
    let store = Arc::new(Corrupting(LocalFileStorage::new(&dest.0, "/files")));
    let report = copy_dir_to_storage(&src.0, store, 2).await.unwrap();
    assert_eq!(report.copied, 0, "{report:?}");
    assert_eq!(report.failed.len(), files.len(), "{report:?}");
    assert!(
        report.failed.iter().all(|(_, why)| why.contains("SHA-256")),
        "{report:?}"
    );
}

/// A tree of files to migrate: nested directories, an image derivative, a
/// file that is not valid UTF-8 content, and the things that must not be
/// copied (the write check's sentinel, a local write's temporary file and a
/// symbolic link to a file outside the directory).
fn uploads_tree(root: &Path, outside: &Path) -> Vec<(String, Vec<u8>)> {
    let files = vec![
        (
            "2026/10/a_report.pdf".to_string(),
            b"%PDF-1.4 report".to_vec(),
        ),
        ("2026/11/b_photo.jpg".to_string(), awkward_bytes()),
        (
            "acme/2026/11/c_note.txt".to_string(),
            b"tenant note".to_vec(),
        ),
        (
            "styles/w400/2026/11/b_photo.jpg".to_string(),
            vec![0xff, 0xd8, 0xff],
        ),
        ("empty.txt".to_string(), Vec::new()),
    ];
    for (key, data) in &files {
        let path = root.join(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    std::fs::write(root.join(".trovato-write-check-0197"), b"sentinel").unwrap();
    std::fs::write(root.join("2026/11/.trovato-tmp-0197"), b"partial").unwrap();
    std::fs::write(outside, b"must never be copied").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside, root.join("2026/link.txt")).unwrap();
    // A link to a directory outside: its files must not be walked into.
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.parent().unwrap(), root.join("2026/linkdir")).unwrap();
    files
}

/// Copy, check every byte, run again: the second run copies nothing.
async fn migration_copies_everything_once(dest: Arc<dyn FileStorage>) {
    let src = TempDir::new("migrate-src");
    let outside = TempDir::new("migrate-outside");
    let files = uploads_tree(&src.0, &outside.0.join("secret.txt"));

    let report = copy_dir_to_storage(&src.0, dest.clone(), 4).await.unwrap();
    assert_eq!(report.total, files.len() as u64, "{report:?}");
    assert_eq!(report.copied, files.len() as u64, "{report:?}");
    assert_eq!(report.already_there, 0);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    for (key, data) in &files {
        assert_eq!(
            &dest.read(&format!("local://{key}")).await.unwrap(),
            data,
            "{key}"
        );
    }
    for skipped in [
        "local://.trovato-write-check-0197",
        "local://2026/11/.trovato-tmp-0197",
        "local://2026/link.txt",
        "local://2026/linkdir/secret.txt",
    ] {
        assert!(
            !dest.exists(skipped).await.unwrap(),
            "{skipped} must not be copied"
        );
    }

    let again = copy_dir_to_storage(&src.0, dest.clone(), 4).await.unwrap();
    assert_eq!(again.copied, 0, "{again:?}");
    assert_eq!(again.already_there, files.len() as u64, "{again:?}");
    assert!(again.failed.is_empty());

    for (key, _) in &files {
        dest.delete(&format!("local://{key}")).await.unwrap();
    }
}

/// An interrupted run is finished by running again: a file already copied
/// is kept, a file the store holds with different bytes (a copy cut short, or
/// one changed since) is copied again, and a file that cannot be copied is
/// reported by name without stopping the rest.
async fn migration_resumes_and_reports(dest: Arc<dyn FileStorage>) {
    let src = TempDir::new("migrate-resume");
    let outside = TempDir::new("migrate-outside");
    let files = uploads_tree(&src.0, &outside.0.join("secret.txt"));
    let (done_key, done_data) = &files[0];
    let (stale_key, stale_data) = &files[1];
    dest.write(&format!("local://{done_key}"), done_data)
        .await
        .unwrap();
    dest.write(&format!("local://{stale_key}"), b"cut short")
        .await
        .unwrap();
    // A name no storage key may have: refused by the store, reported.
    std::fs::write(src.0.join("2026/bad\\name.txt"), b"x").unwrap();

    let report = copy_dir_to_storage(&src.0, dest.clone(), 2).await.unwrap();
    assert_eq!(report.total, files.len() as u64 + 1, "{report:?}");
    assert_eq!(report.already_there, 1, "{report:?}");
    assert_eq!(report.copied, files.len() as u64 - 1, "{report:?}");
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0, "2026/bad\\name.txt");
    assert_eq!(
        &dest.read(&format!("local://{stale_key}")).await.unwrap(),
        stale_data,
        "a stale copy is replaced"
    );
    for (key, _) in &files {
        dest.delete(&format!("local://{key}")).await.unwrap();
    }
}

#[tokio::test]
async fn migration_to_a_directory_copies_everything_once() {
    let dest = TempDir::new("migrate-dest");
    migration_copies_everything_once(Arc::new(LocalFileStorage::new(&dest.0, "/files"))).await;
}

#[tokio::test]
async fn migration_to_a_directory_resumes_and_reports() {
    let dest = TempDir::new("migrate-dest");
    migration_resumes_and_reports(Arc::new(LocalFileStorage::new(&dest.0, "/files"))).await;
}

#[tokio::test]
async fn migration_to_s3_copies_everything_once() {
    let Some(config) = s3_config() else { return };
    migration_copies_everything_once(s3_storage(&config)).await;
}

#[tokio::test]
async fn migration_to_s3_resumes_and_reports() {
    let Some(config) = s3_config() else { return };
    migration_resumes_and_reports(s3_storage(&config)).await;
}

/// The constructor takes no credential from Trovato's own settings: a store
/// builds from the `S3_*` settings alone (the client asks the standard AWS
/// sources when a request is made).
#[test]
fn s3_storage_builds_from_settings_alone() {
    let storage = S3FileStorage::new(
        &S3Config {
            bucket: "trovato-files".into(),
            region: Some("eu-central-1".into()),
            endpoint: None,
            prefix: Some("/site-a/".into()),
            allow_http: false,
        },
        "/files",
    )
    .unwrap();
    assert_eq!(storage.describe(), "s3://trovato-files/site-a");
    assert_eq!(storage.public_url("local://a/b.jpg"), "/files/a/b.jpg");
}

/// A subdirectory that cannot be listed (a root-owned `lost+found` on a mount)
/// is reported by name, and every other file is still copied.
#[cfg(unix)]
#[tokio::test]
async fn migration_reports_an_unreadable_directory_and_goes_on() {
    use std::os::unix::fs::PermissionsExt;
    let src = TempDir::new("migrate-unreadable");
    let dest = TempDir::new("migrate-dest");
    std::fs::create_dir_all(src.0.join("2026")).unwrap();
    std::fs::write(src.0.join("2026/a.txt"), b"a").unwrap();
    let locked = src.0.join("lost+found");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("x.txt"), b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable_anyway = std::fs::read_dir(&locked).is_ok(); // running as root
    let store = Arc::new(LocalFileStorage::new(&dest.0, "/files"));
    let report = copy_dir_to_storage(&src.0, store.clone(), 2).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();
    assert_eq!(store.read("local://2026/a.txt").await.unwrap(), b"a");
    if !readable_anyway {
        assert_eq!(report.copied, 1, "{report:?}");
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(report.failed[0].0, "lost+found/");
    }
}
