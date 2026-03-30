# Story 43.2: Personal Data Flag on Field Definitions

Status: ready-for-dev

## Story

As a **plugin developer building data export/deletion functionality**,
I want field definitions to indicate which fields contain personal data,
so that I can automatically find all PII in the system without per-content-type configuration.

## Acceptance Criteria

1. `FieldDefinition` in `crates/plugin-sdk/src/types.rs` gains `personal_data: bool` field (default `false` via `#[serde(default)]`)
2. Admin UI for field management (`/admin/structure/types/{type}/fields`) includes a "Contains personal data" checkbox
3. Field definitions serialized to YAML via `config export` include `personal_data` when `true`
4. `config import` accepts `personal_data` on field definitions
5. `ContentTypeRegistry` exposes method `personal_data_fields(item_type: &str) -> Vec<&str>` returning field names marked as PII
6. Existing field definitions default to `personal_data: false` (no migration needed -- `#[serde(default)]` handles deserialization)
7. At least 1 integration test: define a content type with PII fields, query `personal_data_fields()`

## Tasks / Subtasks

- [ ] Add `personal_data: bool` field with `#[serde(default)]` to `FieldDefinition` in plugin-sdk (AC: #1, #6)
- [ ] Add "Contains personal data" checkbox to admin field management form template (AC: #2)
- [ ] Update field management route handler to read/write `personal_data` flag (AC: #2)
- [ ] Verify `config export` includes `personal_data` when true (AC: #3)
- [ ] Verify `config import` accepts `personal_data` on field definitions (AC: #4)
- [ ] Add `personal_data_fields(item_type)` method to `ContentTypeRegistry` (AC: #5)
- [ ] Write integration test: register content type with PII-marked fields, call `personal_data_fields()` (AC: #7)

## Dev Notes

### Architecture

- This is a metadata marker -- the kernel does not act on it. Export and deletion plugins use it to find PII automatically.
- `#[serde(default)]` means existing serialized definitions without the field deserialize as `false` -- fully backward compatible, no migration needed.
- `personal_data_fields()` filters `FieldDefinition` entries where `personal_data == true` and returns their names.

### Security

- The `personal_data` flag is informational metadata for compliance tooling. Marking a field does not change its storage or access control.
- Admin-only: only users with content type management permissions can set the flag.

### Testing

- Integration test using `ContentTypeRegistry` to register a type with mixed PII/non-PII fields, then assert `personal_data_fields()` returns only the marked fields.
- Verify round-trip: export config with `personal_data: true`, import it, confirm flag persists.

### References

- [Source: docs/ritrovo/epic-13-privacy.md -- Story 43.2]
- [Source: crates/plugin-sdk/src/types.rs -- FieldDefinition struct]
- [Source: crates/kernel/src/content/type_registry.rs -- ContentTypeRegistry]
