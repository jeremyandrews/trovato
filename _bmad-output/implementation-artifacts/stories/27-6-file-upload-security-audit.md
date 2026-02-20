# Story 27.6: File Upload Security Audit

Status: ready-for-dev

## Story

As a **security reviewer**,
I want file upload handling audited for security vulnerabilities,
So that uploaded files cannot be used to compromise the server or other users.

## Acceptance Criteria

1. File type validation verified — content-based (magic bytes), not just extension
2. Path traversal prevention verified — crafted filenames cannot escape upload directory
3. Executable file blocking verified — dangerous extensions (.php, .sh, .exe, etc.) rejected
4. Upload size limits verified — enforced before full file receipt
5. All findings documented with severity ratings
6. All Critical/High findings fixed

## Tasks / Subtasks

- [ ] Add magic byte validation for uploaded files (AC: #1)
  - [ ] Add `infer` crate (or equivalent) to validate file content matches declared MIME type
  - [ ] Verify image files contain valid image headers
  - [ ] Reject files where extension/Content-Type disagrees with magic bytes
- [ ] Verify path traversal prevention (AC: #2)
  - [ ] Confirm `sanitize_filename()` strips path components via `Path::file_name()`
  - [ ] Confirm character whitelist (`a-z A-Z 0-9 . - _`)
  - [ ] Confirm length limit (200 chars)
  - [ ] Confirm UUID+timestamp URI generation prevents collisions
  - [ ] Deduplicate `sanitize_filename()` — exists in both `service.rs` and `storage.rs`
- [ ] Verify executable file blocking (AC: #3)
  - [ ] Confirm MIME type allowlist (16 types) blocks all executable extensions
  - [ ] Confirm SVG explicitly excluded (XSS prevention)
  - [ ] Verify `guess_mime_type()` cannot be bypassed via double extensions
- [ ] Verify upload size limits (AC: #4)
  - [ ] Confirm 10MB limit enforced at route handler level (early check)
  - [ ] Confirm 10MB limit enforced at service layer (defense in depth)
  - [ ] Confirm 413 Payload Too Large response code
- [ ] Add Content-Disposition header for file serving (AC: #5)
  - [ ] Set `Content-Disposition: attachment` for non-image/non-PDF file types
  - [ ] Or configure in web server (nginx/Caddy) for uploads directory
- [ ] Fix unauthenticated file metadata endpoint (AC: #5)
  - [ ] `GET /file/{id}` returns file metadata without auth — add ownership or auth check
- [ ] Document all findings with severity ratings (AC: #5)

## Dev Notes

### Dependencies

No dependencies on other stories. Can be worked independently.

### Codebase Research Findings

#### HIGH: No Magic Byte Validation

**Location:** `crates/kernel/src/file/service.rs:128-227`, `crates/kernel/src/routes/file.rs:369-389`

File type validation relies entirely on:
1. Extension-based MIME type guessing (`guess_mime_type()`)
2. Client-provided Content-Type header from multipart form

No content-based validation (magic bytes). A `.jpg` file could actually contain executable code or polyglot content. The `infer` crate provides lightweight magic byte detection for common file formats.

#### MEDIUM: No Content-Disposition Headers

Files are served directly from disk by the web server (nginx/Caddy) without explicit `Content-Disposition` headers set by the application. If MIME type validation is bypassed, files could render inline in the browser as HTML (XSS risk). Non-image files should be served with `Content-Disposition: attachment`.

#### MEDIUM: Unauthenticated File Metadata Endpoint

**Location:** `crates/kernel/src/routes/file.rs:200-223`

`GET /file/{id}` returns JSON with file metadata (filename, MIME type, size, URL) without authentication. Allows enumeration of all uploaded files and their download URLs.

#### LOW: Duplicate `sanitize_filename()` Function

**Location:** `crates/kernel/src/file/service.rs:450-470` and `crates/kernel/src/file/storage.rs:141-159`

Same function duplicated in two files. Violates DRY. Should be consolidated to a single location.

#### PROTECTED: MIME Type Allowlist

**Location:** `crates/kernel/src/file/service.rs:15-36`

Strong allowlist of 16 MIME types:
- Images: JPEG, PNG, GIF, WebP
- Documents: PDF, Word, Excel, text, CSV
- Archives: ZIP, GZIP
- SVG explicitly excluded (comment: "XML-based format enables stored XSS")

#### PROTECTED: Path Traversal Prevention

**Location:** `crates/kernel/src/file/service.rs:450-470`

`sanitize_filename()` properly handles traversal:
- `Path::file_name()` extracts only filename component (strips `../`)
- Character whitelist: only `a-z A-Z 0-9 . - _`
- Length capped at 200 chars
- UUID+timestamp prefix in URI prevents collision and enumeration

Test coverage exists: `sanitize_filename("../../etc/passwd")` → `"passwd"`

#### PROTECTED: Upload Size Limits

10MB hard limit enforced at two levels:
1. Route handler level: early check, returns 413 (file.rs:82-96)
2. Service layer: defense-in-depth check (service.rs:136-142)

#### PROTECTED: Authentication on Upload

Both upload endpoints require authentication:
- `POST /file/upload` — checks `SESSION_USER_ID` (file.rs:54)
- `POST /api/block-editor/upload` — checks `SESSION_USER_ID` (file.rs:238)

#### PROTECTED: Image Style Path Validation

**Location:** `crates/kernel/src/routes/image_style.rs`

Image derivative serving validates path components, rejecting `..` and `.` segments.

### Key Files

- `crates/kernel/src/routes/file.rs` — Upload endpoints, MIME type guessing, file metadata
- `crates/kernel/src/file/service.rs` — FileService, upload logic, MIME allowlist, sanitize_filename
- `crates/kernel/src/file/storage.rs` — Storage abstraction, local/S3 backends, duplicate sanitize_filename
- `crates/kernel/src/routes/image_style.rs` — Image derivative serving with path validation

### References

- [Source: crates/kernel/src/routes/file.rs — Upload handlers and MIME guessing]
- [Source: crates/kernel/src/file/service.rs — FileService, allowlist, sanitization]
- [Source: crates/kernel/src/file/storage.rs — Storage backends]
- [Source: crates/kernel/src/routes/image_style.rs — Image style path validation]
