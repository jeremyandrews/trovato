# Story 36.6: ritrovo_access Plugin

Status: done

## Story

As a **site administrator**,
I want stage-based access control for conferences,
so that incoming and curated conferences are only visible to users with appropriate editorial permissions.

## Acceptance Criteria

1. Plugin registers 7 conference-specific permissions via `tap_perm`
2. `tap_item_access` enforces stage-based view permissions: "incoming" requires `view incoming conferences` or `edit conferences`, "curated" requires `view curated conferences` or `edit conferences`
3. Live stage and no-stage items return `Neutral` (kernel handles default access)
4. Non-view operations (edit, delete, update) require `edit conferences` regardless of stage
5. Non-conference items always return `Neutral`
6. `tap_item_view` is a placeholder for future editor_notes field stripping (returns empty)
7. Users with only view permissions on internal stages are denied mutation operations

## Tasks / Subtasks

- [x] Register 7 permissions via tap_perm (view incoming, view curated, edit, publish, post comments, edit own comments, edit any comments) (AC: #1)
- [x] Implement tap_item_access with stage-based view logic (AC: #2, #3)
- [x] Add operation-aware branching for non-view operations (AC: #4, #7)
- [x] Early return Neutral for non-conference items (AC: #5)
- [x] Implement tap_item_view placeholder (AC: #6)
- [x] Write unit tests for all access control scenarios (AC: #1-#7)

## Dev Notes

### Architecture

The plugin (325 lines including tests) implements two taps:
- **`tap_item_access`**: The main access control gate. Routes through a two-level decision: first by operation (view vs. non-view), then by stage. The kernel already denies anonymous users on internal stages, so this tap only handles authenticated user permission checks.
- **`tap_item_view`**: Placeholder that returns empty. Future enhancement will strip `editor_notes` for non-editors once `UserContext` is passed to view taps.

Helper function `has_any_permission()` checks if any of the required permissions exist in the user's permission set.

Note: `publish conferences` is declared but not checked in `tap_item_access` -- publish is a workflow transition enforced by the kernel's workflow engine, not an item access operation.

### Testing

16 unit tests covering: permission count/uniqueness, neutral for non-conference, neutral for live/no-stage, deny/grant incoming, deny/grant curated, deny edit with only view permission, grant edit with edit permission, deny/grant delete, deny/grant update, neutral edit for non-conference, view placeholder.

### References

- `plugins/ritrovo_access/src/lib.rs` (325 lines) -- Full plugin implementation with tests
