# Story 7.4: Stage-Aware Gathers & Search

Status: ready-for-dev

## Story

As a site visitor,
I want public listings and search results to show only published content,
So that draft and in-review content is not exposed.

## Acceptance Criteria

1. Anonymous gather listings show only Live/Public items
2. Editor gather listings include all visible stages
3. Anonymous search returns only Live/Public items
4. Stage-scoped cache: live=bare keys, non-live=st:{stage_id}:{key}

## Tasks / Subtasks

- [ ] Verify/implement stage-aware CTE wrapper on gather queries (AC: #1, #2)
- [ ] Verify search filters by stage visibility (AC: #3)
- [ ] Implement stage-scoped cache keys (AC: #4)

## Dev Notes

- Gather queries: `stage_aware: true` in definitions enables CTE wrapping
- Search: filter by item stage visibility in search query
- Cache: `moka::sync::Cache` — key prefix for non-live stages

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 3]
