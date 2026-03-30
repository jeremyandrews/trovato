# Story 40.2: Image Block Alt Text Schema Enforcement

Status: ready-for-dev

## Story

As a **content platform**,
I want the image block schema to require the `alt` field,
so that every image block in the system carries alt text metadata (even if empty for decorative images).

## Acceptance Criteria

1. Image block JSON schema adds `alt` to the `required` array alongside `file`
2. Migration updates existing image blocks that lack `alt` field to add `alt: ""` (empty string -- treats them as decorative until editors add real alt text)
3. Block editor UI shows `alt` as a required field with helper text: "Describe the image for screen readers. Leave empty for decorative images."
4. API validation rejects image blocks without `alt` field (not without alt *value* -- empty string is valid)
5. Existing `trovato-test` code blocks in tutorials that create image blocks updated to include `alt`
6. `templates/elements/block--image.html` renders `alt="{{ block.alt | default(value='') }}"` (verify or fix)

## Tasks / Subtasks

- [ ] Add `"alt"` to image block's `required` vec in `block_types.rs` (AC: #1)
  - [ ] Verify image block schema definition in `crates/kernel/src/content/block_types.rs`
  - [ ] Add `"alt"` to the `required` array alongside `"file"`
- [ ] Write migration to backfill `alt: ""` on existing image blocks (AC: #2)
  - [ ] Update `item.fields` JSONB where image blocks lack `alt` key
  - [ ] Update `item_revision.fields` JSONB where image blocks lack `alt` key
  - [ ] Test migration on both empty and populated databases
- [ ] Update block editor form to show alt as required (AC: #3)
  - [ ] Verify `crates/kernel/src/content/form.rs` renders alt field for image blocks
  - [ ] Add helper text for the alt field
- [ ] Verify API validation rejects image blocks without `alt` field (AC: #4)
  - [ ] Empty string `alt: ""` must pass validation
  - [ ] Missing `alt` key must fail validation
- [ ] Update `trovato-test` tutorial blocks that create image blocks (AC: #5)
  - [ ] Grep for image block creation in tutorial parts 3 and 5
  - [ ] Add `alt` field to any image block examples that omit it
- [ ] Verify `block--image.html` renders alt attribute correctly (AC: #6)
  - [ ] Check `templates/elements/block--image.html` for alt rendering

## Dev Notes

### Architecture
- `crates/kernel/src/content/block_types.rs` -- image block schema definition with `required` array
- `crates/kernel/src/content/form.rs` -- block editor form rendering
- `templates/elements/block--image.html` -- image block template
- Migration file for JSONB backfill of `alt: ""` on existing image blocks

### Security
- No XSS risk: alt text is rendered inside an HTML attribute with Tera autoescape enabled
- Validation enforces field presence at the schema level, not just UI level

### Testing
- Unit test: image block validation accepts `alt: ""` and `alt: "Description"`
- Unit test: image block validation rejects missing `alt` field
- Integration test: create image block via API without alt, verify rejection
- Migration test: verify backfill adds `alt: ""` to existing blocks

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] -- Epic 40 definition
- [Source: crates/kernel/src/content/block_types.rs] -- Block type schema definitions
- [Source: crates/kernel/src/content/form.rs] -- Block editor form rendering
- [Source: docs/design/Design-Content-Model.md] -- Content model design
