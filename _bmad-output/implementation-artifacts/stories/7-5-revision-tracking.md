# Story 7.5: Revision Tracking

Status: ready-for-dev

## Story

As a site editor,
I want every edit to create a new revision and to be able to revert to a previous version,
So that content history is preserved and mistakes can be undone.

## Acceptance Criteria

1. Edit creates new revision in item_revision table
2. Previous revision preserved unchanged
3. Revert creates NEW revision with old content (never deletes)
4. Tag-based cache invalidation on save

## Tasks / Subtasks

- [ ] Verify revision creation on item save (AC: #1, #2)
- [ ] Verify revert creates new revision (AC: #3)
- [ ] Verify cache invalidation (AC: #4)

## Dev Notes

- Revisions: `crates/kernel/src/content/item_service.rs` — `get_revisions()`, `revert_to_revision()`
- Already implemented — this story validates and demonstrates
- Cache invalidation: tag-based, fires on item save

### References

- [Source: docs/design/Design-Content-Model.md] — revision design
