# Story 2.1: Speaker Content Type Definition

Status: ready-for-dev

## Story

As a site editor,
I want a Speaker content type with bio, company, website, photo, and conferences fields,
So that I can create and manage speaker profiles.

## Acceptance Criteria

1. Speaker content type YAML config imported successfully
2. "Speaker" appears as content type option in admin form
3. Form includes fields: bio (TextLong), company (Text), website (Text), photo (File), conferences (RecordReference -> conference, multi-value)
4. Speaker item created with all fields stored correctly in database

## Tasks / Subtasks

- [ ] Create `item_type.speaker.yml` config file (AC: #1)
  - [ ] Define field_bio (TextLong), field_company (Text), field_website (Text)
  - [ ] Define field_photo (File type)
  - [ ] Define field_conferences (RecordReference -> conference, cardinality: multiple)
- [ ] Import config: `cargo run --release --bin trovato -- config import docs/tutorial/config` (AC: #1)
- [ ] Verify speaker type in `/api/content-types` (AC: #2)
- [ ] Create speaker via admin UI, verify JSONB storage (AC: #3, #4)

## Dev Notes

### Architecture

- Content type registry: `crates/kernel/src/content/type_registry.rs`
- Config import: `crates/kernel/src/config_storage/yaml.rs`
- RecordReference field type exists in `crates/plugin-sdk/src/types.rs` -- FieldType::RecordReference
- Item type config format: see existing `item_type.conference.yml` from Part 1 for pattern
- `/api/content-types` returns `Vec<String>` (just type names)
- Item `title` is a column on `item` table, NOT in JSONB `fields`

### Key Files

- `crates/plugin-sdk/src/types.rs` -- FieldType, FieldDefinition, ContentTypeDefinition
- `crates/kernel/src/content/type_registry.rs` -- ContentTypeRegistry
- `crates/kernel/src/config_storage/yaml.rs` -- config import logic

### References

- [Source: docs/design/Design-Content-Model.md] -- content model design
- [Source: docs/tutorial/plan-parts-03-04.md#Step 3] -- speaker content type
