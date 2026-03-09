# Story 7.7: Emergency Unpublish

Status: ready-for-dev

## Story

As a site administrator,
I want to immediately remove a Live item from public view without stage transition,
So that problematic content can be taken down instantly.

## Acceptance Criteria

1. Setting `active=false` removes item from public listings and detail
2. No stage transition occurs
3. Action recorded in revision history
4. Setting `active=true` restores visibility

## Tasks / Subtasks

- [ ] Implement/verify active flag toggle on Live items (AC: #1, #2)
- [ ] Verify revision history records the change (AC: #3)
- [ ] Verify restoration (AC: #4)

## Dev Notes

- `active` field on item or item_revision controls visibility
- Emergency unpublish: bypass workflow, immediate effect
- Gathers and search must filter by active=true

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 4]
