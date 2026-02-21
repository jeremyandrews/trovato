# Story 29.1: Define `conference` Item Type

Status: ready-for-dev

## Story

As a **developer building a Trovato site**,
I want to define a `conference` Item Type with all fields,
so that I can create and store conference data.

## Acceptance Criteria

1. Item Type definition file (`.info.toml` or equivalent) declares `conference` with all 18 fields from the Ritrovo content model
2. Migration creates the appropriate database schema (item_type row, JSONB field definitions)
3. JSONB fields are correctly typed: `TextValue` (plain and filtered_html), Date, Boolean, Category reference (multi), File (image/pdf), RecordReference (multi)
4. Nullable fields are marked optional where specified (city, country, cfp_url, cfp_end_date, editor_notes, etc.)
5. File fields (logo, venue_photos, schedule_pdf) and RecordReference fields (speakers) are declared but don't need to be fully functional yet (wired in Part 3)
6. `source_id` field exists as a TextValue for dedup key
7. The `conference` Item Type is loadable via Trovato's ItemType API after migration
8. Tutorial Step 2 documentation written in `docs/tutorial/part-01-hello-trovato.md` covering field definitions and JSONB mapping

## Tasks / Subtasks

- [ ] Define the `conference` Item Type configuration (AC: #1)
  - [ ] Create item type definition with all 18 fields
  - [ ] Set field types, required/optional, default values
- [ ] Create database migration for the conference type (AC: #2, #3, #4)
  - [ ] item_type row with machine name `conference`
  - [ ] Field definitions with correct JSONB types
- [ ] Declare file/reference fields as placeholders (AC: #5)
  - [ ] logo (image, required for Live stage)
  - [ ] venue_photos (image, multi)
  - [ ] schedule_pdf (pdf)
  - [ ] speakers (RecordReference to `speaker` type, multi)
- [ ] Add source_id dedup field (AC: #6)
- [ ] Verify ItemType loads correctly via API (AC: #7)
- [ ] Write tutorial documentation for Step 2 (AC: #8)
  - [ ] Field definition syntax explanation
  - [ ] JSONB storage layout explanation
  - [ ] Under the Hood: JSONB storage details

## Dev Notes

### Dependencies

- Epic 4 (Content Modeling & Basic CRUD) provides ItemType table, JSONB fields, and CRUD operations -- all complete
- Epic 8 (Content Categorization) provides Category field type -- complete
- Epic 11 (File & Media Management) provides File field type -- complete
- No new kernel changes required; this story uses existing infrastructure to define a new content type

### Key Files

- `docs/ritrovo/overview.md` -- Master Ritrovo content model reference
- `docs/tutorial/part-01-hello-trovato.md` -- Tutorial chapter (new)
- Item Type definition location TBD (code-defined vs config file vs migration)
- `crates/kernel/src/models/item_type.rs` -- ItemType model reference

### Content Model Reference

| Field | Type | Required | Notes |
|---|---|---|---|
| `name` | TextValue (plain) | Yes | Title field |
| `url` | TextValue (plain) | No | Conference website |
| `start_date` | Date | Yes | |
| `end_date` | Date | Yes | |
| `city` | TextValue (plain) | No | Nullable for online-only |
| `country` | TextValue (plain) | No | Nullable for online-only |
| `online` | Boolean | No | Default false |
| `cfp_url` | TextValue (plain) | No | |
| `cfp_end_date` | Date | No | |
| `description` | TextValue (filtered_html) | No | WYSIWYG later; plain text for now |
| `topics` | Category ref (multi) | No | Declared; taxonomy created in Part 2 |
| `logo` | File (image) | No* | Required for Live stage (Part 3) |
| `venue_photos` | File (image, multi) | No | Part 3 |
| `schedule_pdf` | File (pdf) | No | Part 3 |
| `speakers` | RecordReference (multi) | No | Part 3 |
| `language` | TextValue (plain) | No | ISO 639-1 |
| `source_id` | TextValue (plain) | No | Dedup key |
| `editor_notes` | TextValue (plain) | No | Internal, editors only |

### References

- [Source: docs/ritrovo/overview.md#Content Model]
- [Source: docs/design/Design-Content-Model.md]

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
