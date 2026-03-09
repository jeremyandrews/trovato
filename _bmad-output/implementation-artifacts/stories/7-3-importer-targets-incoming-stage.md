# Story 7.3: Importer Targets Incoming Stage

Status: ready-for-dev

## Story

As a site administrator,
I want imported conferences to land on the Incoming stage instead of Live,
So that imported content goes through editorial review before publication.

## Acceptance Criteria

1. New imports created with Incoming stage
2. Existing Live conferences not affected

## Tasks / Subtasks

- [ ] Update ritrovo_importer plugin: set stage to Incoming on create (AC: #1)
- [ ] Rebuild and reinstall plugin
- [ ] Verify new imports on Incoming (AC: #1)
- [ ] Verify existing Live items unchanged (AC: #2)

## Dev Notes

- ritrovo_importer: `plugins/ritrovo_importer/`
- Plugin uses host function to create items — needs stage parameter
- Plugin directory still on disk from Part 2 (removed from workspace in recent refactor)
- May need to re-add to workspace Cargo.toml temporarily for rebuild

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 3]
