# Story 2.2: RecordReference Rendering on Detail Pages

Status: ready-for-dev

## Story

As a site visitor,
I want to see linked conferences on a speaker page and linked speakers on a conference page,
So that I can navigate between related content.

## Acceptance Criteria

1. Speaker detail page shows referenced conferences as clickable links (forward reference)
2. Conference detail page shows speakers in a "Speakers" section (reverse reference)
3. Deleted references are not displayed (no broken links)

## Tasks / Subtasks

- [ ] Create `templates/elements/item--speaker.html` with conference links section (AC: #1)
  - [ ] Iterate over `field_conferences` RecordReference values
  - [ ] Render each as linked title with URL
- [ ] Update `templates/elements/item--conference.html` with speakers section (AC: #2)
  - [ ] Query reverse references: speakers referencing this conference
  - [ ] Render as linked list in "Speakers" section
- [ ] Handle deleted/missing references gracefully (AC: #3)
- [ ] Integration test: forward and reverse references render correctly

## Dev Notes

### Architecture

- RecordReference stores `target_id` (UUID) and `target_type` (string) in JSONB
- Forward reference: speaker.field_conferences -> list of conference UUIDs
- Reverse reference: query speakers where field_conferences contains this conference ID
- Reverse lookup may use Gather system or direct query via item_service
- Templates access fields via Tera context: `fields.field_conferences`

### Security

- All rendered titles must be HTML-escaped (Tera autoescape handles this)
- Reference URLs should use URL aliases when available

### References

- [Source: docs/design/Design-Content-Model.md] -- RecordReference field type
- [Source: crates/plugin-sdk/src/types.rs] -- RecordReference in FieldType enum
