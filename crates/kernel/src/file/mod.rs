//! File and media management.
//!
//! Provides file upload, storage, and cleanup functionality.

pub mod relocate;
pub mod s3;
pub mod service;
pub mod sigv4;
pub mod storage;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

pub use service::{
    ALLOWED_MIME_TYPES, EXTENSION_MIME_TYPES, FileInfo, FileService, FileStatus, FileVisibility,
    MAX_FILE_SIZE, MAX_UPLOAD_BODY_SIZE, MediaScope, StoreFile, UploadRejected, UploadResult,
    mime_for_filename, resolve_mime_type,
};
pub use relocate::{MigrateSummary, MigrationBackends, migrate_files};
pub use s3::{S3Settings, S3Storage};
pub use storage::{FileStorage, LocalFileStorage, check_writable};

/// Build the storage backend for one scheme (`"public"` or `"private"`),
/// named by `backend` (`"local"` or `"s3"`). The one place `main.rs` and
/// `state.rs` turn `Config`'s file settings into a `FileStorage`, so the two
/// boot paths (server, `trovato config` CLI) cannot drift apart.
///
/// An S3 backend used for both schemes needs its bytes kept apart even
/// though they share one bucket and one set of credentials: `scheme` is
/// appended to the configured key prefix (`{prefix}public/`,
/// `{prefix}private/`), so the two never collide without the operator having
/// to configure two buckets or two prefixes by hand.
pub fn build_storage(
    scheme: &str,
    backend: &str,
    local_dir: &Path,
    files_url: &str,
    s3: Option<&S3Settings>,
) -> Result<Arc<dyn FileStorage>> {
    match backend {
        "s3" => {
            let settings = s3
                .cloned()
                .ok_or_else(|| anyhow::anyhow!(
                    "{scheme}_FILE_BACKEND=s3 but S3_BUCKET is not set; configure S3_BUCKET, \
                     S3_ACCESS_KEY_ID and S3_SECRET_ACCESS_KEY, or set the backend to local"
                ))?;
            let mut settings = settings;
            settings.key_prefix = format!("{}{scheme}/", settings.key_prefix);
            Ok(Arc::new(S3Storage::new(settings)?))
        }
        "local" => Ok(Arc::new(LocalFileStorage::new(local_dir, files_url))),
        other => anyhow::bail!("unknown {scheme} file backend {other:?}: expected local or s3"),
    }
}
