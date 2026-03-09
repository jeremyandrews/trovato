# Story 7.8: Extensibility Demo — Config-Only Stage Addition

Status: ready-for-dev

## Story

As a tutorial reader,
I want to see a new "Legal Review" stage added using only config changes,
So that I understand the system's extensibility without code changes.

## Acceptance Criteria

1. "Legal Review" stage added via YAML config
2. New transitions: curated→legal_review, legal_review→live
3. No code changes or plugin rebuilds required
4. Original transitions continue working alongside new ones

## Tasks / Subtasks

- [ ] Create `tag.legal_review.yml` — Internal, weight 15 (AC: #1)
- [ ] Update workflow config with new transitions (AC: #2)
- [ ] Import config only — no code changes (AC: #3)
- [ ] Verify new and existing transitions work (AC: #4)

## Dev Notes

- Config-only: YAML import adds stage tag and workflow transitions
- No code, no plugin rebuild — demonstrates extensibility
- Weight 15 puts Legal Review between Curated (10) and Live (20)

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 3]
