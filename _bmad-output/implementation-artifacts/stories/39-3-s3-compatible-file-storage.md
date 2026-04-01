# Story 39.3: S3-Compatible File Storage

Status: done

## Story

As a **platform operator**,
I want file uploads stored via a pluggable storage backend supporting both local filesystem and S3-compatible object storage,
so that the system can scale to multi-server deployments without shared filesystem dependencies.

## Acceptance Criteria

1. `FileStorage` trait defined with `write`, `read`, `delete`, `exists`, `public_url`, and `scheme` methods
2. `LocalFileStorage` implementation stores files at `local://{year}/{month}/{uuid}_{filename}` URIs
3. `S3FileStorage` implementation stores files in S3-compatible buckets with tenant-scoped key prefixes
4. Directory traversal prevented: `LocalFileStorage::parse_uri()` rejects `..` path components
5. Filename sanitization via `sanitize_filename()` before URI generation
6. Security enforced in `FileService`: 10MB max upload size, MIME type allowlist, magic byte validation
7. Tenant-scoped URIs: file paths include date-based directory structure for organization

## Tasks / Subtasks

- [x] Define `FileStorage` async trait with write/read/delete/exists/public_url/scheme methods (AC: #1)
- [x] Implement `LocalFileStorage` with base_path and base_url configuration (AC: #2)
- [x] Implement `LocalFileStorage::parse_uri()` with directory traversal protection (AC: #4)
- [x] Implement `LocalFileStorage::generate_uri()` with date-based path and UUIDv7 uniqueness (AC: #2, #7)
- [x] Implement `S3FileStorage` using AWS SDK for S3-compatible storage (AC: #3)
- [x] Integrate `sanitize_filename()` into URI generation pipeline (AC: #5)
- [x] Implement file upload validation in `FileService`: size limits, MIME allowlist, magic byte checks (AC: #6)
- [x] Add unit tests for storage backends and validation (AC: #1, #4, #5, #6)

## Dev Notes

### Architecture

The `FileStorage` trait provides a backend-agnostic interface for file operations. `LocalFileStorage` maps `local://` URIs to filesystem paths relative to a configured base directory. `S3FileStorage` maps `s3://` URIs to S3 object keys with a configurable bucket and prefix. Both implementations are `Send + Sync` for use in async contexts.

File security is layered:
- `sanitize_filename()` strips dangerous characters and normalizes the filename
- `validate_magic_bytes()` checks that file content matches the declared MIME type, rejecting disguised executables (ELF/PE binaries with image MIME types)
- `ALLOWED_MIME_TYPES` allowlist prevents upload of executable content types
- 10MB size limit prevents resource exhaustion

The `FileService` (661 lines) orchestrates uploads by combining validation, storage, and database record creation.

### Testing

- Unit tests in `crates/kernel/src/file/storage.rs` (4 tests) -- local storage URI parsing, traversal rejection
- Unit tests in `crates/kernel/src/file/service.rs` (9 tests) -- filename sanitization, MIME validation, magic bytes
- Integration tests exercise full upload/download flows

### References

- `crates/kernel/src/file/storage.rs` (403 lines) -- FileStorage trait, LocalFileStorage, S3FileStorage
- `crates/kernel/src/file/service.rs` (661 lines) -- FileService, validation, sanitize_filename, validate_magic_bytes
