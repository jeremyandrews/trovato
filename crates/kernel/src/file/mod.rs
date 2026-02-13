//! File and media management.
//!
//! Provides file upload, storage, and cleanup functionality.

pub mod service;
pub mod storage;

pub use service::{
    ALLOWED_MIME_TYPES, FileInfo, FileService, FileStatus, MAX_FILE_SIZE, UploadResult,
};
pub use storage::{FileStorage, LocalFileStorage};

#[cfg(feature = "s3")]
pub use storage::S3FileStorage;
