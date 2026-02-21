# Story 29.2: Admin UI for Manual Conference Creation

Status: ready-for-dev

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

- [ ] Verify auto-generated form renders for the `conference` type (AC: #1, #3, #4)
  - [ ] Confirm all fields appear with correct widgets
  - [ ] Confirm date pickers work for start_date, end_date, cfp_end_date
  - [ ] Confirm online checkbox renders correctly
- [ ] Verify form validation (AC: #2, #7)
  - [ ] Required field validation for name, start_date, end_date
  - [ ] CSRF token present and validated
- [ ] Verify Item creation and storage (AC: #5, #6)
  - [ ] Item stored with correct JSONB field values
  - [ ] Success message with link to view Item
- [ ] Create seed conference data (AC: #8)
  - [ ] Conference 1: A major Rust conference (e.g., RustConf 2026)
  - [ ] Conference 2: A European conference (non-US data)
  - [ ] Conference 3: An online-only conference (exercises `online` boolean)
  - [ ] At least one with CFP URL and end date
- [ ] Write tutorial Step 3 documentation (AC: #9)
  - [ ] Walk through creating a conference manually
  - [ ] Show raw database row (JSONB inspection)
  - [ ] Explain Item IDs and timestamps
  - [ ] Foreshadow Stages (everything in default stage for now)

## Dev Notes

### Dependencies

- Story 29.1 (Define conference Item Type) must be complete
- Epic 4 Story 4-11 (Auto-generated Admin Forms) provides the form infrastructure -- complete
- Epic 16 (Admin Interface Completion) provides admin list/edit UI -- complete

### Key Files

- `crates/kernel/src/routes/admin_content.rs` -- content creation handlers
- `templates/admin/content/` -- content form templates
- `docs/tutorial/part-01-hello-trovato.md` -- Tutorial chapter
- Seed data script or migration TBD

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

### Debug Log References

### Completion Notes List

### File List
