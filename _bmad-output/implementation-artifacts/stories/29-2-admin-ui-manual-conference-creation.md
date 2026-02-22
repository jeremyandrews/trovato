# Story 29.2: Admin UI for Manual Conference Creation

Status: done

## Story

As a **site administrator**,
I want to create conferences through a web form,
so that I can populate the site with content.

## Acceptance Criteria

1. Auto-generated admin form at `/admin/content/add/conference` renders all `conference` fields with appropriate widgets
2. Required fields (name, start_date, end_date) are validated on submission
3. Date fields use date picker widgets
4. Boolean field (`online`) renders as a checkbox
5. Created Items are stored in JSONB and retrievable via the Item API
6. Success message shown after creation with link to view the created Item
7. CSRF protection on the creation form
8. At least 3 real conferences manually created as seed data for the Gather story (RustConf or equivalent, one European conference, one online-only conference)
9. Tutorial Step 3 documentation written covering manual content creation, JSONB storage inspection, and Item IDs

## Tasks / Subtasks

- [x] Verify auto-generated form renders for the `conference` type (AC: #1, #3, #4)
  - [x] Confirm all fields appear with correct widgets
  - [x] Confirm date pickers work for start_date, end_date, cfp_end_date
  - [x] Confirm online checkbox renders correctly
- [x] Verify form validation (AC: #2, #7)
  - [x] Required field validation for name, start_date, end_date
  - [x] CSRF token present and validated
- [x] Verify Item creation and storage (AC: #5, #6)
  - [x] Item stored with correct JSONB field values
  - [x] Success message with link to view Item
- [x] Create seed conference data (AC: #8)
  - [x] Conference 1: RustConf 2026 (Portland, OR, USA)
  - [x] Conference 2: EuroRust 2026 (Paris, France)
  - [x] Conference 3: WasmCon Online 2026 (online-only, exercises `online` boolean)
  - [x] RustConf has CFP URL and end date
- [x] Write tutorial Step 3 documentation (AC: #9)
  - [x] Walk through creating a conference manually
  - [x] Show raw database row (JSONB inspection)
  - [x] Explain Item IDs and timestamps
  - [x] Foreshadow Stages (everything in default stage for now)

## Dev Notes

### Dependencies

- Story 29.1 (Define conference Item Type) must be complete
- Epic 4 Story 4-11 (Auto-generated Admin Forms) provides the form infrastructure -- complete
- Epic 16 (Admin Interface Completion) provides admin list/edit UI -- complete

### Key Files

- `crates/kernel/src/routes/admin_content.rs` -- content creation handlers
- `templates/admin/content-form.html` -- content form template
- `templates/admin/content-list.html` -- content list template (flash messages)
- `docs/tutorial/part-01-hello-trovato.md` -- Tutorial chapter
- `crates/kernel/migrations/20260224000002_seed_conference_items.sql` -- Seed migration

### Testing

- Verify form renders with correct fields
- Verify required field validation
- Verify successful Item creation
- Verify JSONB storage structure matches field definitions

### References

- [Source: docs/ritrovo/overview.md#Content Model]
- [Source: docs/ritrovo/epic-01.md#Step 3: Create Content Manually]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

### Completion Notes List

- Added Date, File, RecordReference, Email, Float field type handlers to content-form.html template
- Added `title_label: Option<String>` to `ContentTypeDefinition` in plugin-sdk, propagated to all plugins and type_registry methods
- Template uses `{{ content_type.title_label | default(value="Title") }}` for dynamic title label
- Seed migration creates 3 conferences (RustConf 2026, EuroRust 2026, WasmCon Online 2026) with UUIDv7 IDs, idempotent guard
- Flash success message added to content creation/update/delete handlers using session-based pattern from plugin_admin.rs
- Tutorial Step 3 covers form walkthrough, JSONB storage, UUIDv7 IDs, timestamps, stages foreshadowing
- 12 integration tests covering all ACs: form rendering, date pickers, checkboxes, JSONB storage, required field validation, CSRF rejection, flash create/update/delete messages, boolean absence/presence, seed data verification
- Adversarial review fixes: boolean type consistency (string "1" not JSON true/false), per-conference idempotency guards, URL aliases for seeded items, File field renders as disabled placeholder, CSRF test asserts exact 400 status, validation test asserts specific error text, flash messages use content type label not machine name, flash failures logged, `resolve_title_label` helper extracts duplicated fallback logic, tutorial uses unique conference name to avoid seed data collision

### File List

- `crates/plugin-sdk/src/types.rs` -- Added `title_label: Option<String>` to `ContentTypeDefinition`
- `crates/kernel/src/content/type_registry.rs` -- Propagated `title_label` through `sync_from_plugins`, `register_type`, `create`, `update` methods
- `crates/kernel/src/routes/admin_content.rs` -- Added flash success messages for create/update/delete
- `crates/kernel/src/content/form.rs` -- Added `title_label: None` to test helpers
- `templates/admin/content-form.html` -- Added Date/File/RecordReference/Email/Float handlers, dynamic title label
- `templates/admin/content-list.html` -- Added flash message display
- `crates/kernel/migrations/20260224000002_seed_conference_items.sql` -- New: idempotent seed migration with UUIDv7
- `crates/kernel/tests/integration_test.rs` -- 12 new conference tests (including update/delete flash)
- `crates/kernel/tests/item_test.rs` -- Added `title_label: None` to test helpers
- `docs/tutorial/part-01-hello-trovato.md` -- Tutorial Step 3: manual conference creation
- `plugins/argus/src/lib.rs` -- Added `title_label: None` to 7 content type definitions
- `plugins/blog/src/lib.rs` -- Added `title_label: None` to 1 content type definition
- `plugins/goose/src/lib.rs` -- Added `title_label: None` to 5 content type definitions
- `plugins/media/src/lib.rs` -- Added `title_label: None` to 1 content type definition
- `plugins/netgrasp/src/lib.rs` -- Added `title_label: None` to 6 content type definitions
