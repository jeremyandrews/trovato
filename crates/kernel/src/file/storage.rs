//! File storage backends.
//!
//! Provides trait and implementations for storing files locally or in S3.

use std::path::PathBuf;

use crate::file::service::sanitize_filename;
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

    /// Delete a file from storage.
    async fn delete(&self, uri: &str) -> Result<()>;

    /// Check if a file exists.
    async fn exists(&self, uri: &str) -> Result<bool>;

    /// Get the public URL for a file.
    fn public_url(&self, uri: &str) -> String;

    /// Get the storage scheme (e.g., "local", "s3").
    fn scheme(&self) -> &'static str;
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

    /// Generate a storage URI for a new file.
    pub fn generate_uri(&self, filename: &str) -> String {
        let now = chrono::Utc::now();
        let year = now.format("%Y");
        let month = now.format("%m");
        let unique_id = uuid::Uuid::now_v7().simple().to_string();
        let safe_filename = sanitize_filename(filename);

        format!(
            "local://{}/{}/{}_{}",
            year,
            month,
            &unique_id[..8],
            safe_filename
        )
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

    fn public_url(&self, uri: &str) -> String {
        let path = uri.strip_prefix("local://").unwrap_or(uri);
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    fn scheme(&self) -> &'static str {
        "local"
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

/// S3-compatible object storage.
#[cfg(feature = "s3")]
pub struct S3FileStorage {
    client: aws_sdk_s3::Client,
    bucket: String,
    /// Optional prefix for all keys.
    prefix: Option<String>,
    /// Base URL for public access (e.g., CloudFront distribution).
    base_url: String,
}

#[cfg(feature = "s3")]
impl S3FileStorage {
    /// Create a new S3 file storage.
    ///
    /// Uses the default AWS credential chain (env vars, config file, instance profile).
    pub async fn new(
        bucket: impl Into<String>,
        prefix: Option<String>,
        base_url: impl Into<String>,
    ) -> Result<Self> {
        let config = aws_config::load_from_env().await;
        let client = aws_sdk_s3::Client::new(&config);

        Ok(Self {
            client,
            bucket: bucket.into(),
            prefix,
            base_url: base_url.into(),
        })
    }

    /// Create with a custom endpoint (for S3-compatible services like MinIO).
    pub async fn with_endpoint(
        endpoint_url: &str,
        bucket: impl Into<String>,
        prefix: Option<String>,
        base_url: impl Into<String>,
    ) -> Result<Self> {
        let config = aws_config::from_env()
            .endpoint_url(endpoint_url)
            .load()
            .await;
        let client = aws_sdk_s3::Client::new(&config);

        Ok(Self {
            client,
            bucket: bucket.into(),
            prefix,
            base_url: base_url.into(),
        })
    }

    /// Parse an s3:// URI to get the S3 key.
    fn parse_uri(&self, uri: &str) -> Result<String> {
        let path = uri
            .strip_prefix("s3://")
            .context("invalid S3 URI, must start with s3://")?;

        match &self.prefix {
            Some(prefix) => Ok(format!("{}/{}", prefix.trim_end_matches('/'), path)),
            None => Ok(path.to_string()),
        }
    }

    /// Generate a storage URI for a new file.
    pub fn generate_uri(&self, filename: &str) -> String {
        let now = chrono::Utc::now();
        let year = now.format("%Y");
        let month = now.format("%m");
        let unique_id = uuid::Uuid::now_v7().simple().to_string();
        let safe_filename = sanitize_filename(filename);

        format!(
            "s3://{}/{}/{}_{}",
            year,
            month,
            &unique_id[..8],
            safe_filename
        )
    }
}

#[cfg(feature = "s3")]
#[async_trait]
impl FileStorage for S3FileStorage {
    async fn write(&self, uri: &str, data: &[u8]) -> Result<()> {
        let key = self.parse_uri(uri)?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(aws_sdk_s3::primitives::ByteStream::from(data.to_vec()))
            .send()
            .await
            .context("failed to upload to S3")?;

        debug!(uri = %uri, key = %key, size = data.len(), "file written to S3");
        Ok(())
    }

    async fn read(&self, uri: &str) -> Result<Vec<u8>> {
        let key = self.parse_uri(uri)?;

        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .context("failed to get object from S3")?;

        let data = response
            .body
            .collect()
            .await
            .context("failed to read S3 response body")?
            .into_bytes()
            .to_vec();

        debug!(uri = %uri, size = data.len(), "file read from S3");
        Ok(data)
    }

    async fn delete(&self, uri: &str) -> Result<()> {
        let key = self.parse_uri(uri)?;

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .context("failed to delete from S3")?;

        debug!(uri = %uri, "file deleted from S3");
        Ok(())
    }

    async fn exists(&self, uri: &str) -> Result<bool> {
        let key = self.parse_uri(uri)?;

        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(err) => {
                // Check if it's a "not found" error
                if let Some(service_err) = err.as_service_error() {
                    if service_err.is_not_found() {
                        return Ok(false);
                    }
                }
                Err(err).context("failed to check S3 object existence")
            }
        }
    }

    fn public_url(&self, uri: &str) -> String {
        let path = uri.strip_prefix("s3://").unwrap_or(uri);
        match &self.prefix {
            Some(prefix) => format!(
                "{}/{}/{}",
                self.base_url.trim_end_matches('/'),
                prefix.trim_end_matches('/'),
                path
            ),
            None => format!("{}/{}", self.base_url.trim_end_matches('/'), path),
        }
    }

    fn scheme(&self) -> &'static str {
        "s3"
    }
}

#[cfg(feature = "s3")]
impl std::fmt::Debug for S3FileStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3FileStorage")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
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
    fn test_generate_uri() {
        let storage = LocalFileStorage::new("/tmp/uploads", "/files");
        let uri = storage.generate_uri("test.jpg");

        assert!(uri.starts_with("local://"));
        assert!(uri.ends_with("_test.jpg"));
    }

    #[test]
    fn test_public_url() {
        let storage = LocalFileStorage::new("/tmp/uploads", "https://example.com/files");
        let url = storage.public_url("local://2026/02/abc123_test.jpg");

        assert_eq!(url, "https://example.com/files/2026/02/abc123_test.jpg");
    }
}
