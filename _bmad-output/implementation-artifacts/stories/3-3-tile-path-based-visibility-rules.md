# Story 3.3: Tile Path-Based Visibility Rules

Status: ready-for-dev

## Story

As a site administrator,
I want Tiles to appear only on pages matching specific URL path patterns,
So that contextually relevant content blocks appear on the right pages.

## Acceptance Criteria

1. CFP sidebar Tile with paths `/conferences*`, `/topics/*`, `/cfps*` renders on `/conferences/rustconf-2026`
2. Same Tile NOT rendered on `/speakers/jane-doe`
3. Tiles with no visibility restrictions render on all pages

## Tasks / Subtasks

- [ ] Configure visibility paths on `tile.open_cfps_sidebar.yml` (AC: #1, #2)
- [ ] Implement path matching in tile rendering logic (AC: #1, #2)
- [ ] Verify unrestricted tiles render everywhere (AC: #3)

## Dev Notes

### Architecture

- Tile visibility: path pattern matching (glob-style or prefix matching)
- Visibility rules stored in tile config (visibility_paths field)
- Rendering: page template checks tile visibility before rendering
- Existing tile model should have visibility fields — verify in `models/tile.rs`

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Step 4] — visibility rules
