# Story 43.3: User Data Export Tap

Status: ready-for-dev

## Story

As a **GDPR compliance plugin developer**,
I want a `tap_user_export` hook,
so that plugins can contribute their data to a user's data portability export.

## Acceptance Criteria

1. New `tap_user_export` tap added to the tap registry
2. Tap signature: `tap_user_export(user_id: Uuid) -> UserExportData` where `UserExportData` contains `plugin_name: String`, `data_type: String`, `records: Vec<serde_json::Value>`
3. Kernel aggregates `UserExportData` from all plugins that implement `tap_user_export`
4. Kernel provides a `/api/v1/user/export` endpoint (authenticated -- users can only export their own data; admins can export any user's data)
5. Export format: JSON (machine-readable per GDPR Article 20)
6. Export includes: user profile fields, all items authored by the user, all comments by the user, all file uploads by the user, plus plugin-contributed data
7. Items include only fields marked `personal_data: true` plus title and metadata (not all fields)
8. Export endpoint is rate-limited (1 request per hour per user -- exports can be expensive)
9. At least 2 integration tests: export with no plugins, export with a mock plugin contributing data

## Tasks / Subtasks

- [ ] Define `UserExportData` type in `crates/plugin-sdk/src/types.rs` (AC: #2)
- [ ] Add `tap_user_export` to the tap registry (AC: #1)
- [ ] Implement tap invocation that aggregates `UserExportData` from all responding plugins (AC: #3)
- [ ] Build core user data export logic: profile fields, authored items (PII-only), comments, files (AC: #6, #7)
- [ ] Filter item fields to only those marked `personal_data: true` plus title and metadata (AC: #7)
- [ ] Add `/api/v1/user/export` route with authentication and authorization checks (AC: #4)
- [ ] Return JSON export response (AC: #5)
- [ ] Add rate limiting: 1 request per hour per user (AC: #8)
- [ ] Write integration test: export with no plugins returns core user data (AC: #9)
- [ ] Write integration test: export with mock plugin contributing data (AC: #9)

## Dependencies

**Blocked by:** Story 43.2 (Personal Data Flag on Field Definitions) — this story's AC #7 requires `personal_data_fields()` from 43.2. Story 43.2 must be completed first.

## Dev Notes

### Architecture

- Hard dependency on Story 43.2 (`personal_data` flag on field definitions) for filtering item fields in exports. Do not start this story until 43.2 is merged.
- The kernel handles core data export (user profile, items, comments, files). Plugins add their own data via the tap.
- Export structure:

  ```json
  {
    "user": { "id": "...", "username": "...", ... },
    "items": [ { "title": "...", "personal_fields": {...} } ],
    "comments": [...],
    "files": [...],
    "plugin_data": [
      { "plugin_name": "...", "data_type": "...", "records": [...] }
    ]
  }
  ```

- JSON is the simplest and most portable format. CSV or XML export would be plugin territory.

### Security

- Authentication required: anonymous users cannot export data.
- Authorization: users can only export their own data. Users with admin permission can export any user's data.
- Rate limiting prevents abuse: 1 export request per hour per user.
- Items include only `personal_data`-flagged fields to avoid leaking non-personal content in exports.

### Testing

- Test with no plugins: verify core data (user profile, items, comments, files) is present.
- Test with mock plugin: verify `plugin_data` array includes plugin-contributed records.
- Test authorization: non-admin user cannot export another user's data.
- Test rate limiting: second request within an hour returns 429.

### References

- [Source: docs/ritrovo/epic-13-privacy.md -- Story 43.3]
- [Source: crates/kernel/src/tap/ -- Tap registry]
- [Source: crates/plugin-sdk/src/types.rs -- SDK types]
- [Source: crates/kernel/src/routes/api_v1.rs -- API routes]
