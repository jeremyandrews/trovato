//! File and media management.
//!
//! Provides file upload, storage, and cleanup functionality.

pub mod service;
pub mod storage;

pub use service::{FileInfo, FileService, FileStatus, UploadResult, ALLOWED_MIME_TYPES, MAX_FILE_SIZE};
pub use storage::{FileStorage, LocalFileStorage};

#[cfg(feature = "s3")]
pub use storage::S3FileStorage;
