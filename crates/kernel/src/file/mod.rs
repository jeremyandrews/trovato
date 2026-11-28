//! File and media management.
//!
//! Provides file upload, storage, and cleanup functionality.

pub mod migrate;
pub mod range;
pub mod service;
pub mod storage;

pub use range::{ByteRange, RangeNotSatisfiable, is_range_not_satisfiable, parse_range_header};
pub use service::{
    ALLOWED_MIME_TYPES, EXTENSION_MIME_TYPES, FileInfo, FileService, FileStatus, MAX_FILE_SIZE,
    MAX_UPLOAD_BODY_SIZE, MediaScope, StoreFile, UploadRejected, UploadResult, mime_for_filename,
    resolve_mime_type,
};
pub use storage::{
    FileStorage, FileStream, LocalFileStorage, REMOTE_STARTUP_CHECK_LIMIT, S3Config,
    S3FileStorage, StorageConfig, StorageNotFound, StoreUnreachable, build_storage,
    check_writable, is_not_found, is_unreachable, memory_stream, startup_check,
};
