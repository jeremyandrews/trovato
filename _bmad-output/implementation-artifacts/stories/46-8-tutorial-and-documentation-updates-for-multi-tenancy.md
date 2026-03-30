# Story 46.8: Tutorial and Documentation Updates for Multi-Tenancy

Status: ready-for-dev

## Story

As a **tutorial reader**,
I want multi-tenancy mentioned naturally where relevant,
so that I understand the concept exists without being overwhelmed by details.

## Acceptance Criteria

1. Tutorial Part 1 includes a brief mention when explaining `stage_id`: "Every item also has a `tenant_id` -- like `stage_id`, it defaults to a built-in value and is invisible for single-site deployments"
2. Tutorial Part 2 requires no changes
3. Tutorial Part 4 includes a brief mention: "Stages are per-tenant"
4. Other tutorial parts require no changes unless clearly warranted
5. Recipe files for Parts 1 and 4 are updated to reflect any tutorial text changes
6. Sync hashes in updated recipes are recalculated and updated

## Tasks / Subtasks

- [ ] Add one-sentence `tenant_id` mention in Tutorial Part 1 where `stage_id` is explained (AC: #1)
- [ ] Add one-sentence mention in Tutorial Part 4 that stages are per-tenant (AC: #3)
- [ ] Verify Tutorial Part 2 needs no changes (AC: #2)
- [ ] Verify other tutorial parts need no changes (AC: #4)
- [ ] Update Part 1 recipe to reflect the tutorial text change (AC: #5)
- [ ] Update Part 4 recipe to reflect the tutorial text change (AC: #5)
- [ ] Recalculate and update sync hashes for modified recipes (AC: #6)

## Dev Notes

### Architecture

This is a documentation-only story. No code changes. The additions are intentionally minimal -- one sentence each in two tutorial parts. The goal is awareness, not instruction: readers should know `tenant_id` exists and that it's similar to `stage_id` in being a default-value column they don't need to think about for single-site use.

### Testing

- Run `bash docs/tutorial/recipes/sync-check.sh` after updates to verify all sync hashes are correct.
- Read the modified paragraphs in context to ensure they flow naturally and don't disrupt the tutorial narrative.

### References

- `docs/tutorial/` -- tutorial source files
- `docs/tutorial/recipes/` -- companion agent recipes
- `docs/tutorial/recipes/sync-check.sh` -- sync hash verification script
