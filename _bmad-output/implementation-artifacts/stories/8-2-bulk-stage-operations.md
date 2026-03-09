# Story 8.2: Bulk Stage Operations

Status: ready-for-dev

## Story

As a site editor,
I want to select multiple items and change their stage in bulk,
So that I can process batches of content efficiently.

## Acceptance Criteria

1. Multi-select checkboxes on content list
2. "Change stage" bulk action with target stage selection
3. Workflow permissions respected per item (skip unauthorized with warning)
4. Summary displayed: N transitioned, M skipped
5. Empty selection shows validation error

## Tasks / Subtasks

- [ ] Add checkboxes and bulk action dropdown to content list (AC: #1, #2)
- [ ] Implement bulk stage change with per-item permission check (AC: #3)
- [ ] Display summary (AC: #4)
- [ ] Validate non-empty selection (AC: #5)

## Dev Notes

- CSRF required on bulk POST endpoint
- Each item checked individually against workflow transitions and permissions
- Use `require_csrf` from `crate::routes::helpers`

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 5]
