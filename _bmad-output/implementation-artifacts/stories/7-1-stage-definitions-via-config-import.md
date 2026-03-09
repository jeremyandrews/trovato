# Story 7.1: Stage Definitions via Config Import

Status: ready-for-dev

## Story

As a site administrator,
I want to define editorial stages via YAML config,
So that the content workflow is reproducible and version-controlled.

## Acceptance Criteria

1. Three stages imported: Incoming (Internal, default), Curated (Internal), Live (Public)
2. Incoming marked as default stage for new content
3. Re-importing is idempotent

## Tasks / Subtasks

- [ ] Create stage config YAML files (tags in "stages" category) (AC: #1)
- [ ] Import config (AC: #1)
- [ ] Verify default stage assignment (AC: #2)
- [ ] Verify idempotency (AC: #3)

## Dev Notes

- Stages: vocabulary-based, tags in `stages` category
- `crates/kernel/src/models/stage.rs`, `stage/mod.rs`
- Stages fully implemented — this story configures via YAML, not code
- Live stage may have well-known UUID

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 3]
