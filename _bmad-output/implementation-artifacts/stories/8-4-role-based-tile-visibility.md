# Story 8.4: Role-Based Tile Visibility

Status: ready-for-dev

## Story

As a site administrator,
I want Tile visibility rules to support role-based conditions in addition to path-based rules,
So that certain Tiles are shown only to specific user roles.

## Acceptance Criteria

1. Tile with role "editor" rendered for editors on matching paths
2. Same tile NOT rendered for anonymous on same path
3. Both path AND role conditions must be satisfied
4. Tiles with no role restriction work as before (backward compatible)

## Tasks / Subtasks

- [ ] Add role visibility field to Tile model/config (AC: #1, #2)
- [ ] Update tile rendering to check user role (AC: #1, #2, #3)
- [ ] Verify backward compatibility — path-only tiles unaffected (AC: #4)
- [ ] Create `tile.editor_tools.yml` — editor-only sidebar tile
- [ ] Create `templates/tiles/editor-tools.html`

## Dev Notes

- Tile model: `crates/kernel/src/models/tile.rs` — add role visibility field
- Tile rendering: check both path match AND role match
- Editor tools tile: quick links to content list, pending imports

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 5]
