# Story 3.1: Base Page Layout with Slot Regions

Status: ready-for-dev

## Story

As a site visitor,
I want every page to share a consistent layout with header, navigation, content, sidebar, and footer areas,
So that the site feels cohesive and professional.

## Acceptance Criteria

1. Base `page.html` template defines five named Slot regions: Header, Navigation, Content, Sidebar, Footer
2. Content slot contains page-specific content
3. Other slots populated by assigned Tiles
4. Empty slots collapse gracefully (no empty divs rendered)

## Tasks / Subtasks

- [ ] Modify `templates/page.html` to define five Slot regions (AC: #1)
  - [ ] Each region iterates over tiles assigned to it
  - [ ] Tiles rendered in weight order within region
- [ ] Verify Content slot renders page-specific content (AC: #2)
- [ ] Verify empty slots collapse (AC: #4)

## Dev Notes

### Architecture

- Slots are named regions rendered in page template via Tera blocks
- Tile rendering: iterate tiles for each slot, ordered by weight (lower = higher position)
- Existing: `crates/kernel/src/models/tile.rs`, `routes/tile_admin.rs`
- Template: `templates/page.html` (or `templates/base.html`)

### References

- [Source: docs/design/Design-Web-Layer.md] — Slots & Tiles architecture
- [Source: docs/tutorial/plan-parts-03-04.md#Step 4] — Slot configuration
