//! File storage backends.
//!
//! Provides the [`FileStorage`] trait and its two backends:
//! [`LocalFileStorage`], a directory (the default, for one server or a shared
//! volume), and [`S3FileStorage`], any S3 compatible object store (AWS S3,
//! MinIO, and the S3 APIs of other providers), so any number of servers can
//! share uploads without a shared filesystem.
//!
//! A stored file's URI is `local://<key>` with either backend. The scheme names
//! the site's file store, not the medium: item bodies, file fields and
//! `file_managed` rows keep the same URI when a site moves its files from a
//! directory to a bucket (`trovato files migrate-to-s3`), so nothing in the
//! database is rewritten. The move can be repeated while writes are stopped,
//! and undone only before the first upload or delete on the bucket: after that
//! the directory still holds files deleted since, which would be served with no
//! row (docs/cli-reference.md, `trovato files migrate-to-s3`).
//!
//! Neither backend hands out a URL into the store. Every file is served by
//! Trovato's own `/files/` route, which checks access first
//! (`routes/file.rs`); a CDN in front keeps a copy only of what an anonymous
//! visitor may fetch. So an S3 bucket is never public, and the only principal
//! that can read it is the server's own role (docs/design/production-hosting.md).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

/// The error a backend returns when the file asked for is not stored.
///
/// [`FileService::load_file_data`](super::FileService::load_file_data) turns it
/// into `None`, which the serving routes answer with 404. Any other error is a
/// failure of the store itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageNotFound(pub String);

impl std::fmt::Display for StorageNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "file not found in storage: {}", self.0)
    }
}

impl std::error::Error for StorageNotFound {}

/// Whether `e` says the file is not stored (rather than that the store failed).
pub fn is_not_found(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause.downcast_ref::<StorageNotFound>().is_some()
            || cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

/// The startup check's error when the store could not be reached at all (a
/// connection, timeout or server error), as opposed to a definite answer
/// (no such bucket, access denied, other bytes read back).
///
/// A server refuses to start on a definite answer: that is a setting or
/// permission to fix, and the deploy should fail. On an unreachable store it
/// logs the error and starts, because the store being down is an outage that
/// heals itself, and a pod restarted during it would otherwise crash loop
/// until it ends (and, once every pod had turned over, take reads of the app
/// down with it).
#[derive(Debug)]
pub struct StoreUnreachable(pub String);

impl std::fmt::Display for StoreUnreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreUnreachable {}

/// Whether `e` is [`StoreUnreachable`].
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|cause| cause.downcast_ref::<StoreUnreachable>().is_some())
}

/// File storage backend trait.
#[async_trait]
pub trait FileStorage: Send + Sync {
    /// Write data to storage at the given URI, replacing what was there. A
    /// reader sees the old bytes or the new ones, never a mix.
    async fn write(&self, uri: &str, data: &[u8]) -> Result<()>;

    /// Read data from storage at the given URI. A URI with nothing stored is
    /// a [`StorageNotFound`] error (see [`is_not_found`]).
    async fn read(&self, uri: &str) -> Result<Vec<u8>>;

    /// Delete a file from storage. Deleting a file that is not there succeeds.
    async fn delete(&self, uri: &str) -> Result<()>;

    /// Check if a file exists.
    async fn exists(&self, uri: &str) -> Result<bool>;

    /// Get the public URL for a file.
    fn public_url(&self, uri: &str) -> String;

    /// Get the storage scheme (e.g., "local", "s3").
    fn scheme(&self) -> &'static str;

    /// The directory files are written to, for a storage that is a local
    /// directory: what the status report checks is writable.
    fn local_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// Prove this server can write, read back and delete a file in the store.
    ///
    /// Run once at startup and by the status report, so a store this server
    /// cannot use (a broken mount, a missing bucket, a role without the right
    /// permissions) fails the deploy, not the first upload.
    async fn check_usable(&self) -> Result<()>;

    /// Where files go, for messages: a directory, or a bucket and prefix.
    /// Never includes a credential.
    fn describe(&self) -> String;
}

/// Reject a storage key that could name something outside the store: a `..`
/// or `.` component, an empty component, a leading `/`, a backslash or NUL.
fn checked_key(uri: &str) -> Result<&str> {
    let key = uri
        .strip_prefix("local://")
        .context("invalid storage URI, must start with local://")?;
    if key.is_empty()
        || key.starts_with('/')
        || key.contains('\\')
        || key.contains('\0')
        || key
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        anyhow::bail!("directory traversal not allowed in storage URI");
    }
    Ok(key)
}

/// Prove this server can write, read back and delete a file in `dir`.
///
/// Run once at startup against `UPLOADS_DIR`. A directory this server cannot
/// use would otherwise surface only when someone uploads a file, and on several
/// servers only on the one whose mount is broken. The sentinel's name is unique
/// so servers starting together against a shared volume never touch each
/// other's.
pub async fn check_writable(dir: &std::path::Path) -> Result<()> {
    let fail = |step: &str, e: &dyn std::fmt::Display| {
        anyhow::anyhow!(
            "the uploads directory {} is not usable: could not {step} a test file ({e}). \
             Set UPLOADS_DIR to a directory this server can write, read and delete in \
             (on several servers, the volume they share).",
            dir.display()
        )
    };
    fs::create_dir_all(dir)
        .await
        .map_err(|e| fail("create the directory for", &e))?;
    let sentinel = dir.join(format!(".trovato-write-check-{}", uuid::Uuid::now_v7()));
    let expected = sentinel.to_string_lossy().into_owned();
    fs::write(&sentinel, expected.as_bytes())
        .await
        .map_err(|e| fail("write", &e))?;
    let read = fs::read(&sentinel).await;
    let removed = fs::remove_file(&sentinel).await;
    match read {
        Ok(bytes) if bytes == expected.as_bytes() => {}
        Ok(_) => return Err(fail("read back", &"its contents differed")),
        Err(e) => return Err(fail("read back", &e)),
    }
    removed.map_err(|e| fail("delete", &e))?;
    Ok(())
}

/// Where uploaded files are stored, from `FILE_STORAGE` and the `S3_*`
/// settings. `local` (a directory, `UPLOADS_DIR`) is the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageConfig {
    /// A local directory: `UPLOADS_DIR`.
    Local,
    /// An S3 compatible bucket.
    S3(S3Config),
}

/// The settings of an S3 compatible store.
///
/// Credentials are not here: they come from the standard AWS sources the
/// client reads itself (EKS Pod Identity, IRSA web identity, the instance
/// role, or `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` for a self hosted
/// store such as MinIO), so no key is ever part of Trovato's own settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    /// `S3_BUCKET`.
    pub bucket: String,
    /// `S3_REGION` (else `AWS_REGION`, else `us-east-1`).
    pub region: Option<String>,
    /// `S3_ENDPOINT`: a non-AWS store, such as `http://127.0.0.1:9000`.
    pub endpoint: Option<String>,
    /// `S3_PREFIX`: a key prefix, for a bucket shared with other data.
    pub prefix: Option<String>,
    /// `S3_ALLOW_HTTP`: allow a plain `http://` endpoint (a local MinIO).
    pub allow_http: bool,
}

/// Build the storage `config` names.
pub fn build_storage(
    config: &StorageConfig,
    uploads_dir: &std::path::Path,
    files_url: &str,
) -> Result<std::sync::Arc<dyn FileStorage>> {
    Ok(match config {
        StorageConfig::Local => std::sync::Arc::new(LocalFileStorage::new(uploads_dir, files_url)),
        StorageConfig::S3(s3) => std::sync::Arc::new(S3FileStorage::new(s3, files_url)?),
    })
}

/// An S3 compatible object store.
pub struct S3FileStorage {
    store: object_store::aws::AmazonS3,
    bucket: String,
    prefix: Option<String>,
    base_url: String,
}

impl S3FileStorage {
    /// Build a client for `config`, serving under `base_url` (`FILES_URL`).
    ///
    /// Failures are reported quickly: a request gets 5 seconds to connect and
    /// 30 to finish, and a failed request is retried at most three times while
    /// less than ten seconds have passed. A request is waiting, and a store that
    /// is down for longer is better reported than hidden.
    pub fn new(config: &S3Config, base_url: impl Into<String>) -> Result<Self> {
        use object_store::aws::AmazonS3Builder;
        use object_store::{BackoffConfig, ClientOptions, RetryConfig};

        let mut builder = AmazonS3Builder::from_env().with_bucket_name(&config.bucket);
        if let Some(region) = &config.region {
            builder = builder.with_region(region);
        }
        if let Some(endpoint) = &config.endpoint {
            builder = builder.with_endpoint(endpoint);
        }
        // `allow_http` is a client option: set it on the options, which
        // replace any set before.
        let store = builder
            .with_client_options(
                ClientOptions::new()
                    .with_timeout(Duration::from_secs(30))
                    .with_connect_timeout(Duration::from_secs(5))
                    .with_allow_http(config.allow_http),
            )
            .with_retry(RetryConfig {
                backoff: BackoffConfig::default(),
                max_retries: 3,
                retry_timeout: Duration::from_secs(10),
            })
            .build()
            .with_context(|| format!("failed to configure S3 bucket {}", config.bucket))?;
        let prefix = config
            .prefix
            .as_deref()
            .map(|p| p.trim_matches('/'))
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        Ok(Self {
            store,
            bucket: config.bucket.clone(),
            prefix,
            base_url: base_url.into(),
        })
    }

    /// The object path for `uri`.
    fn object_path(&self, uri: &str) -> Result<object_store::path::Path> {
        let key = checked_key(uri)?;
        let full = match &self.prefix {
            Some(prefix) => format!("{prefix}/{key}"),
            None => key.to_string(),
        };
        object_store::path::Path::parse(&full).context("invalid storage key")
    }

    /// Map a client error, turning "no such key" into [`StorageNotFound`].
    fn map_err(uri: &str, e: object_store::Error, what: &str) -> anyhow::Error {
        match e {
            object_store::Error::NotFound { .. } => StorageNotFound(uri.to_string()).into(),
            other => anyhow::Error::new(other).context(format!("failed to {what} {uri}")),
        }
    }
}

#[async_trait]
impl FileStorage for S3FileStorage {
    async fn write(&self, uri: &str, data: &[u8]) -> Result<()> {
        use object_store::ObjectStoreExt;
        let path = self.object_path(uri)?;
        self.store
            .put(&path, object_store::PutPayload::from(data.to_vec()))
            .await
            .map_err(|e| Self::map_err(uri, e, "write"))?;
        debug!(uri = %uri, size = data.len(), "file written to S3");
        Ok(())
    }

    async fn read(&self, uri: &str) -> Result<Vec<u8>> {
        use object_store::ObjectStoreExt;
        let path = self.object_path(uri)?;
        let got = self
            .store
            .get(&path)
            .await
            .map_err(|e| Self::map_err(uri, e, "read"))?;
        let bytes = got
            .bytes()
            .await
            .map_err(|e| Self::map_err(uri, e, "read"))?;
        Ok(bytes.to_vec())
    }

    async fn delete(&self, uri: &str) -> Result<()> {
        use object_store::ObjectStoreExt;
        let path = self.object_path(uri)?;
        match self.store.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(Self::map_err(uri, e, "delete")),
        }
    }

    async fn exists(&self, uri: &str) -> Result<bool> {
        use object_store::ObjectStoreExt;
        let path = self.object_path(uri)?;
        match self.store.head(&path).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(Self::map_err(uri, e, "check")),
        }
    }

    fn public_url(&self, uri: &str) -> String {
        let path = uri.strip_prefix("local://").unwrap_or(uri);
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    fn scheme(&self) -> &'static str {
        "s3"
    }

    async fn check_usable(&self) -> Result<()> {
        let uri = format!("local://.trovato-write-check-{}", uuid::Uuid::now_v7());
        let fail = |step: &str, e: &anyhow::Error| {
            let message = format!(
                "the file store {} is not usable: could not {step} a test file ({e:#}). \
                 Check S3_BUCKET, S3_REGION and S3_ENDPOINT, and that this server's \
                 role may put, get and delete objects there.",
                self.describe()
            );
            // A transport failure or server error is the store being down;
            // anything else (not found, denied) is a definite answer.
            let transient = e.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<object_store::Error>(),
                    Some(object_store::Error::Generic { .. })
                )
            });
            if transient {
                anyhow::Error::new(StoreUnreachable(message))
            } else {
                anyhow::anyhow!(message)
            }
        };
        self.write(&uri, uri.as_bytes())
            .await
            .map_err(|e| fail("write", &e))?;
        let read = self.read(&uri).await;
        let removed = self.delete(&uri).await;
        match read {
            Ok(bytes) if bytes == uri.as_bytes() => {}
            Ok(_) => {
                return Err(fail("read back", &anyhow::anyhow!("its contents differed")));
            }
            Err(e) => return Err(fail("read back", &e)),
        }
        removed.map_err(|e| fail("delete", &e))?;
        Ok(())
    }

    fn describe(&self) -> String {
        match &self.prefix {
            Some(prefix) => format!("s3://{}/{prefix}", self.bucket),
            None => format!("s3://{}", self.bucket),
        }
    }
}

impl std::fmt::Debug for S3FileStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The client holds credentials; print only where files go.
        f.debug_struct("S3FileStorage")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// Local filesystem storage.
pub struct LocalFileStorage {
    /// Base path for file storage.
    base_path: PathBuf,
    /// Base URL for public file access.
    base_url: String,
}

impl LocalFileStorage {
    /// Create a new local file storage.
    pub fn new(base_path: impl Into<PathBuf>, base_url: impl Into<String>) -> Self {
        Self {
            base_path: base_path.into(),
            base_url: base_url.into(),
        }
    }

    /// Parse a local:// URI to get the path under the base directory.
    ///
    /// Rejects `..`, `.` and empty components and a leading `/` (which
    /// `Path::join` would treat as absolute), so a URI can never name a file
    /// outside the directory.
    fn parse_uri(&self, uri: &str) -> Result<PathBuf> {
        Ok(self.base_path.join(checked_key(uri)?))
    }
}

#[async_trait]
impl FileStorage for LocalFileStorage {
    async fn write(&self, uri: &str, data: &[u8]) -> Result<()> {
        let path = self.parse_uri(uri)?;

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .context("failed to create directories")?;
        }

        // Write beside the target, then rename over it, so a reader on this or
        // another server sees the old bytes or the new ones, never a mix.
        let file_name = path
            .file_name()
            .context("storage URI names no file")?
            .to_string_lossy();
        let temp = path.with_file_name(format!(
            ".{file_name}.trovato-tmp-{}",
            uuid::Uuid::now_v7().simple()
        ));
        let written = async {
            let mut file = fs::File::create(&temp)
                .await
                .context("failed to create file")?;
            file.write_all(data).await.context("failed to write file")?;
            file.flush().await.context("failed to flush file")?;
            fs::rename(&temp, &path)
                .await
                .context("failed to move file into place")
        }
        .await;
        if written.is_err() {
            let _ = fs::remove_file(&temp).await;
        }
        written?;

        debug!(uri = %uri, path = ?path, size = data.len(), "file written");
        Ok(())
    }

    async fn read(&self, uri: &str) -> Result<Vec<u8>> {
        let path = self.parse_uri(uri)?;
        let data = match fs::read(&path).await {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StorageNotFound(uri.to_string()).into());
            }
            Err(e) => return Err(anyhow::Error::new(e).context("failed to read file")),
        };
        debug!(uri = %uri, size = data.len(), "file read");
        Ok(data)
    }

    async fn delete(&self, uri: &str) -> Result<()> {
        let path = self.parse_uri(uri)?;

        if path.exists() {
            fs::remove_file(&path)
                .await
                .context("failed to delete file")?;
            debug!(uri = %uri, "file deleted");
        } else {
            warn!(uri = %uri, "file not found for deletion");
        }

        Ok(())
    }

    async fn exists(&self, uri: &str) -> Result<bool> {
        let path = self.parse_uri(uri)?;
        Ok(path.exists())
    }

    fn public_url(&self, uri: &str) -> String {
        let path = uri.strip_prefix("local://").unwrap_or(uri);
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    fn scheme(&self) -> &'static str {
        "local"
    }

    fn local_dir(&self) -> Option<&std::path::Path> {
        Some(&self.base_path)
    }

    async fn check_usable(&self) -> Result<()> {
        check_writable(&self.base_path).await
    }

    fn describe(&self) -> String {
        self.base_path.display().to_string()
    }
}

impl std::fmt::Debug for LocalFileStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalFileStorage")
            .field("base_path", &self.base_path)
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Tests marked `SECURITY REGRESSION TEST` verify fixes for specific security
    //! findings from Epic 27. Do not remove without security review.

    use super::*;
    use crate::file::service::sanitize_filename;

    // SECURITY REGRESSION TEST — Story 27.6 Finding #2: path traversal prevention
    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("test.jpg"), "test.jpg");
        assert_eq!(sanitize_filename("my file.jpg"), "my_file.jpg");
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("test<script>.jpg"), "test_script_.jpg");
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #2: path traversal attack vectors
    #[test]
    fn test_sanitize_filename_traversal_vectors() {
        // Unix-style traversal
        assert_eq!(sanitize_filename("../../../etc/shadow"), "shadow");
        // Windows-style backslash traversal (backslashes replaced with underscores on Unix)
        let result = sanitize_filename("..\\..\\windows\\system32\\config");
        assert!(!result.contains('\\'), "backslashes should be sanitized");
        // Null byte injection (stripped by Path::file_name)
        let result = sanitize_filename("shell.php\0.jpg");
        assert!(!result.contains('\0'));
        // Double encoding attempt: % is not in the allowed charset, replaced with _
        let result = sanitize_filename("..%2F..%2Fetc%2Fpasswd");
        assert!(!result.contains('%'), "percent signs should be sanitized");
        assert!(!result.contains('/'), "slashes should not appear");
    }

    #[test]
    fn test_public_url() {
        let storage = LocalFileStorage::new("/tmp/uploads", "https://example.com/files");
        let url = storage.public_url("local://2026/02/abc123_test.jpg");

        assert_eq!(url, "https://example.com/files/2026/02/abc123_test.jpg");
    }

    #[tokio::test]
    async fn a_usable_uploads_directory_passes_and_is_left_as_it_was() {
        let dir = std::env::temp_dir().join(format!("trovato-uploads-{}", uuid::Uuid::now_v7()));
        check_writable(&dir)
            .await
            .expect("a fresh directory is usable");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(left.is_empty(), "the sentinel was left behind: {left:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn an_unusable_uploads_directory_is_refused_by_name() {
        // A path under a regular file can never be created.
        let file = std::env::temp_dir().join(format!("trovato-not-a-dir-{}", uuid::Uuid::now_v7()));
        std::fs::write(&file, b"x").unwrap();
        let dir = file.join("uploads");
        let err = check_writable(&dir).await.expect_err("must refuse");
        let message = err.to_string();
        assert!(message.contains(&dir.display().to_string()), "{message}");
        assert!(message.contains("UPLOADS_DIR"), "{message}");
        std::fs::remove_file(&file).unwrap();
    }
}
