# Story 1.3: File Upload on Item Forms

Status: ready-for-dev

## Story

As a site editor,
I want to upload conference logos and venue photos through the item edit form,
So that conference pages display visual media.

## Acceptance Criteria

1. File upload widget appears in conference item edit form
2. Upload validates MIME type against allowlist (NFR2)
3. Upload validates magic bytes match declared MIME type (NFR2)
4. Files larger than 10 MB rejected with clear error (NFR3)
5. Disguised executables (ELF/PE headers with image MIME) rejected (NFR2)
6. Uploaded file associated with item on save

## Tasks / Subtasks

- [ ] Update `item_type.conference.yml` to add `field_logo` (File) and `field_venue_photo` (File) (AC: #1)
- [ ] Re-import config to register new fields
- [ ] Wire file upload widget into auto-generated admin form (AC: #1)
- [ ] Verify MIME allowlist validation (AC: #2)
- [ ] Verify magic byte validation via `validate_magic_bytes()` (AC: #3)
- [ ] Verify 10 MB size limit (AC: #4)
- [ ] Verify ELF/PE disguised executable rejection (AC: #5)
- [ ] Update `item--conference.html` to display logo and venue photo (AC: #6)

## Dev Notes

### Architecture

- File upload infrastructure exists: `crates/kernel/src/file/service.rs`, `file/storage.rs`
- MIME validation: `ALLOWED_MIME_TYPES` allowlist in file service
- Magic byte validation: `validate_magic_bytes()` in file service
- Filename sanitization: `sanitize_filename()` — NEVER use raw user filenames
- File field type exists in plugin-sdk: `FieldType::File`
- Config import: `cargo run --release --bin trovato -- config import docs/tutorial/config`
- Max file size: 10 MB (configurable)

### Security (CRITICAL)

- MUST validate magic bytes against declared MIME type
- MUST use `sanitize_filename()` for all uploaded filenames
- MUST block executable MIME types via allowlist
- MUST reject disguised executables (ELF/PE with image MIME)
- See CLAUDE.md § File Upload Security

### References

- [Source: docs/design/Design-Infrastructure.md] — file management design
- [Source: crates/kernel/src/file/service.rs] — FileService implementation
- [Source: crates/kernel/src/file/storage.rs] — storage backend
