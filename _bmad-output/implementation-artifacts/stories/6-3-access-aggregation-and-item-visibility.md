# Story 6.3: Access Aggregation & Item Visibility

Status: ready-for-dev

## Story

As a site visitor,
I want content access to be determined by combining all plugin access responses,
So that access control is consistent and predictable.

## Acceptance Criteria

1. Any Grant + no Deny = access allowed
2. Any Deny = access denied (regardless of Grants)
3. All Neutral = access denied (default deny)

## Tasks / Subtasks

- [ ] Verify kernel aggregation logic in access check pipeline (AC: #1, #2, #3)
- [ ] Integration test: Grant + no Deny → allowed
- [ ] Integration test: Grant + Deny → denied
- [ ] Integration test: all Neutral → denied

## Dev Notes

- Aggregation in kernel: `crates/kernel/src/content/item_service.rs` — `load_for_view()`
- Access check calls `tap_item_access` on all plugins, aggregates results
- This story validates kernel behavior, not plugin code

### References

- [Source: docs/design/Design-Content-Model.md] — access control design
