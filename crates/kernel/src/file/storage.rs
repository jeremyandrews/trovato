//! File storage backends.
//!
//! Provides the [`FileStorage`] trait, the [`LocalFileStorage`] backend and
//! the [`crate::file::s3::S3Storage`] backend (BL-109, reversed by Jeremy's
//! ruling 2026-11-27: an S3-compatible backend was dropped for the 1.0
//! release because it was an unused, non-default optional feature and the
//! sole remaining carrier of the legacy AWS-SDK TLS chain, rustls 0.21 /
//! webpki 0.101. The workspace is now on rustls 0.23 throughout, so the
//! precondition the old header named is met; the new backend signs requests
//! itself — see `crate::file::sigv4` — rather than bringing an AWS SDK back).

use std::path::PathBuf;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

/// File storage backend trait.
#[async_trait]
pub trait FileStorage: Send + Sync {
    /// Write data to storage at the given URI.
    async fn write(&self, uri: &str, data: &[u8]) -> Result<()>;

    /// Read data from storage at the given URI.
    async fn read(&self, uri: &str) -> Result<Vec<u8>>;

    /// Read data from storage at the given URI as a stream of chunks, for a
    /// download route to pipe straight to the response body rather than
    /// buffering the whole file. The default streams [`Self::read`]'s result
    /// as one chunk; [`crate::file::s3::S3Storage`] overrides this with a
    /// true streamed HTTP response body.
    async fn read_stream(
        &self,
        uri: &str,
    ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<bytes::Bytes>> + Send>>>
    {
        let data = self.read(uri).await?;
        Ok(Box::pin(tokio_stream::once(Ok(bytes::Bytes::from(data)))))
    }

    /// Delete a file from storage.
    async fn delete(&self, uri: &str) -> Result<()>;

    /// Check if a file exists.
    async fn exists(&self, uri: &str) -> Result<bool>;

    /// The size in bytes of the file at `uri`.
    async fn size(&self, uri: &str) -> Result<u64>;

    /// Get the public URL for a file.
    fn public_url(&self, uri: &str) -> String;

    /// A short-lived URL that serves `uri` directly from the backend without
    /// going through the kernel, for a private file this backend can presign
    /// (S3, valid for its configured `presign_ttl`). `Ok(None)` means this
    /// backend has no such mechanism (local storage): the caller must stream
    /// the bytes itself.
    async fn presigned_get_url(&self, uri: &str) -> Result<Option<String>> {
        let _ = uri;
        Ok(None)
    }

    /// Get the storage scheme (e.g., "local", "s3").
    fn scheme(&self) -> &'static str;

    /// A one-line, secret-free description for the status report: backend
    /// kind and location (directory, or endpoint and bucket).
    fn describe(&self) -> String;

    /// The directory files are written to, for a storage that is a local
    /// directory: what the status report checks is writable.
    fn local_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// Prove this backend can write, read back and delete a probe object: the
    /// status report's write/read check, generic over every backend because
    /// it is expressed only in terms of this trait's own methods.
    async fn probe(&self) -> Result<()> {
        let key = format!("_trovato_probe/{}", uuid::Uuid::now_v7());
        let uri = format!("{}://{key}", self.scheme());
        let payload = uri.as_bytes();
        self.write(&uri, payload)
            .await
            .context("could not write a probe object")?;
        let read = self.read(&uri).await;
        let deleted = self.delete(&uri).await;
        let read = read.context("could not read the probe object back")?;
        if read != payload {
            anyhow::bail!("the probe object's contents differed after being read back");
        }
        deleted.context("could not delete the probe object")?;
        Ok(())
    }
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

    /// Parse a local:// URI to get the relative path.
    ///
    /// Rejects paths containing `..` components to prevent directory traversal.
    fn parse_uri(&self, uri: &str) -> Result<PathBuf> {
        let path = uri
            .strip_prefix("local://")
            .context("invalid local URI, must start with local://")?;
        // Reject directory traversal attempts
        for component in std::path::Path::new(path).components() {
            if matches!(component, std::path::Component::ParentDir) {
                anyhow::bail!("directory traversal not allowed in storage URI");
            }
        }
        Ok(self.base_path.join(path))
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

        // Write file
        let mut file = fs::File::create(&path)
            .await
            .context("failed to create file")?;

        file.write_all(data).await.context("failed to write file")?;

        file.flush().await.context("failed to flush file")?;

        debug!(uri = %uri, path = ?path, size = data.len(), "file written");
        Ok(())
    }

    async fn read(&self, uri: &str) -> Result<Vec<u8>> {
        let path = self.parse_uri(uri)?;
        let data = fs::read(&path).await.context("failed to read file")?;
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

    async fn size(&self, uri: &str) -> Result<u64> {
        let path = self.parse_uri(uri)?;
        Ok(fs::metadata(&path)
            .await
            .context("failed to stat file")?
            .len())
    }

    fn public_url(&self, uri: &str) -> String {
        let path = uri.strip_prefix("local://").unwrap_or(uri);
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    fn scheme(&self) -> &'static str {
        "local"
    }

    fn describe(&self) -> String {
        format!("local ({})", self.base_path.display())
    }

    fn local_dir(&self) -> Option<&std::path::Path> {
        Some(&self.base_path)
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
