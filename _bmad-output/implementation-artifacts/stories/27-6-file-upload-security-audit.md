# Story 27.6: File Upload Security Audit

Status: review

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

- [x] Add magic byte validation for uploaded files (AC: #1)
  - [x] Added `infer` crate for content-based file type detection
  - [x] `validate_magic_bytes()` verifies content matches declared MIME type
  - [x] Images, PDF, ZIP, GZIP validated via magic bytes
  - [x] Office XML formats (.docx, .xlsx) validated as ZIP archives
  - [x] Text formats skip validation (no reliable magic bytes)
- [x] Verify path traversal prevention (AC: #2)
  - [x] Confirmed `sanitize_filename()` strips path components via `Path::file_name()`
  - [x] Confirmed character whitelist (`a-z A-Z 0-9 . - _`)
  - [x] Confirmed length limit (200 chars)
  - [x] Confirmed UUID+timestamp URI generation prevents collisions
  - [x] Deduplicated `sanitize_filename()` — consolidated to `service.rs`, imported in `storage.rs`
- [x] Verify executable file blocking (AC: #3)
  - [x] Confirmed MIME type allowlist (13 types) blocks all executable extensions
  - [x] Confirmed SVG explicitly excluded (comment: "XML-based format enables stored XSS")
  - [x] `guess_mime_type()` maps known extensions only; unknown extensions fall through to `application/octet-stream` which is blocked by allowlist
  - [x] Magic byte validation prevents disguised executables (e.g., ELF binary renamed to .jpg)
- [x] Verify upload size limits (AC: #4)
  - [x] Confirmed 10MB limit at route handler level (early check, returns 413)
  - [x] Confirmed 10MB limit at service layer (defense in depth)
- [x] Document all findings with severity ratings (AC: #5, #6)

## Findings Summary

### Fixed (High)

| # | Severity | Location | Issue | Fix |
|---|----------|----------|-------|-----|
| 1 | HIGH | `file/service.rs:upload()` | No content-based file validation. Extension-only MIME check allows uploading executables disguised as images. | Added `validate_magic_bytes()` using `infer` crate. Verifies magic bytes match declared MIME type for images, PDF, ZIP, GZIP, Office XML. |

### Fixed (Low)

| # | Severity | Location | Issue | Fix |
|---|----------|----------|-------|-----|
| 2 | LOW | `file/storage.rs` + `file/service.rs` | Duplicate `sanitize_filename()` function. | Consolidated to `service.rs` as `pub(crate)`, imported in `storage.rs`. |

### Acceptable (Medium/Low)

| # | Severity | Location | Assessment |
|---|----------|----------|------------|
| 3 | MEDIUM | File serving | Files served directly by web server without `Content-Disposition` headers from the application. Configurable at the web server level (nginx/Caddy). With magic byte validation, MIME spoofing is prevented. Document as operational recommendation. |
| 4 | LOW | `GET /file/{id}` | Returns file metadata without auth. UUIDs prevent enumeration. Files are publicly accessible via storage URL. Same finding as Story 27.4 #5. |

### Already Protected

| # | Aspect | Status | Details |
|---|--------|--------|---------|
| 5 | MIME allowlist | PROTECTED | 13 safe MIME types. SVG excluded (XSS). Unknown types blocked. |
| 6 | Path traversal | PROTECTED | `sanitize_filename()`: `Path::file_name()` strips `../`, char whitelist, 200-char limit. UUID prefix prevents collisions. |
| 7 | Upload size | PROTECTED | 10 MB limit at route and service layers. Returns 413 Payload Too Large. |
| 8 | Authentication | PROTECTED | Both upload endpoints require `SESSION_USER_ID`. |
| 9 | Image style paths | PROTECTED | `image_style.rs` rejects `..` and `.` path segments. |

## Implementation Details

### Magic Byte Validation

Added `validate_magic_bytes()` in `file/service.rs` that uses the `infer` crate to detect file type from content. Called after MIME type allowlist check but before storage. Validation strategy by type:

- **Images (JPEG, PNG, GIF, WebP):** Must match exactly
- **PDF:** Must match exactly
- **ZIP/GZIP:** Must match exactly
- **Office XML (.docx, .xlsx):** Validated as ZIP (since OOXML is ZIP-based)
- **Legacy Office (.doc, .xls):** Skipped (OLE format not reliably detected)
- **Text (plain, CSV):** Skipped (no magic bytes)

### sanitize_filename Deduplication

Removed duplicate function from `storage.rs`. Made the `service.rs` version `pub(crate)` and imported it in `storage.rs`. Both production code and tests now use the single implementation.

### Files Changed

- `Cargo.toml` — Added `infer = "0.19"` workspace dependency
- `crates/kernel/Cargo.toml` — Added `infer` dependency
- `crates/kernel/src/file/service.rs` — `validate_magic_bytes()`, `sanitize_filename` now `pub(crate)`, 4 new tests
- `crates/kernel/src/file/storage.rs` — Removed duplicate `sanitize_filename()`, imports from service

### Test Coverage

- 4 new unit tests for magic byte validation (valid PNG, mismatch rejection, text skip, empty binary rejection)
- All 570 unit tests pass
- All 82 integration tests pass
- `cargo clippy --all-targets -- -D warnings` clean
- `cargo fmt --all --check` clean
