# Story 6.4: Field-Level Access via Render Tree

Status: ready-for-dev

## Story

As a site visitor,
I want sensitive fields like editor_notes hidden from non-editors,
So that internal editorial content is not exposed publicly.

## Acceptance Criteria

1. Non-editor viewing item: `editor_notes` stripped from render tree
2. Editor viewing item: `editor_notes` present in output
3. Plugin tag/attribute names validated against SAFE_TAGS/is_valid_attr_key()

## Tasks / Subtasks

- [ ] Implement `tap_item_view` in ritrovo_access plugin (AC: #1, #2)
  - [ ] Check user permissions in request context
  - [ ] Strip `editor_notes` RenderElement if user lacks "edit any conference"
- [ ] Verify SAFE_TAGS validation for plugin-supplied tags (AC: #3)
- [ ] Test: anonymous sees no editor_notes, editor sees them

## Dev Notes

- Render tree manipulation: `tap_item_view` / `tap_item_view_alter`
- SAFE_TAGS: `crates/kernel/src/theme/render.rs`
- `is_valid_attr_key()`: `crates/kernel/src/theme/render.rs`
- Plugin accesses user context via host function for permission check

### References

- [Source: docs/design/Design-Render-Theme.md] — render tree manipulation
- [Source: crates/kernel/src/theme/render.rs] — SAFE_TAGS, validation
