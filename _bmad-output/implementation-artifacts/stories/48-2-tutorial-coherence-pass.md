# Story 48.2: Tutorial Coherence Pass

Status: ready-for-dev

## Story

As a tutorial reader going through Parts 1-7 sequentially,
I want the tutorial to read as a coherent narrative,
so that I can follow the entire sequence without encountering contradictions, stale references, or duplicated explanations.

## Acceptance Criteria

1. No concept is explained twice unless it is intentional progressive disclosure (first mention simplified, later mention deepened), and such cases are clearly signposted.
2. Terminology is consistent across all 7 parts, using Trovato terms per CLAUDE.md (category not taxonomy/vocabulary, item not node, tap not hook, plugin not module, gather not views, tile not block).
3. All forward references ("we'll cover this in Part N") are verified accurate — the referenced part and section exist and cover the claimed topic.
4. All backward references ("as we saw in Part N") are verified accurate — the referenced part and section exist and contain the claimed content.
5. Each part's introduction still sets up its own content and each conclusion still transitions to the next part.
6. Code blocks can be followed sequentially — no step depends on unstated prerequisites or produces output inconsistent with later steps.
7. `trovato-test` blocks pass against the updated kernel.
8. No orphaned instructions remain (steps referencing removed features, old UI paths, or deleted config files).

## Tasks / Subtasks

- [ ] Read Parts 1-7 sequentially, cataloguing every forward and backward reference (AC: #3, #4)
- [ ] Verify each forward reference points to an existing section with matching content (AC: #3)
- [ ] Verify each backward reference points to an existing section with matching content (AC: #4)
- [ ] Search for duplicate concept explanations across all 7 parts; remove or convert to progressive disclosure (AC: #1)
- [ ] Audit all terminology for CLAUDE.md compliance across all 7 parts (AC: #2)
- [ ] Verify each part's intro paragraph and conclusion paragraph form a coherent chain (AC: #5)
- [ ] Walk through code blocks sequentially, verifying each builds on the prior state (AC: #6)
- [ ] Run `trovato-test` blocks against current kernel and fix failures (AC: #7)
- [ ] Search for references to removed features, old UI routes, or deleted files and remove them (AC: #8)

## Dev Notes

### Architecture

This is a pure documentation pass — no kernel code changes. The work is editorial but must be verified against the running system. A systematic approach is critical: build a reference map (forward/backward) before making changes to avoid introducing new inconsistencies while fixing old ones.

### Testing

- Build a cross-reference table: Part N section X references Part M section Y, verified yes/no.
- Run all `trovato-test` blocks against a fresh database restored from the appropriate tutorial backup.
- After all edits, re-read Parts 1-7 sequentially one final time to confirm coherence.

### References

- `docs/tutorial/part-01-hello-trovato.md`
- `docs/tutorial/part-02-content-modeling.md`
- `docs/tutorial/part-03-look-and-feel.md`
- `docs/tutorial/part-04-categories-and-gathers.md`
- `docs/tutorial/part-05-forms-and-input.md`
- `docs/tutorial/part-06-users-and-permissions.md`
- `docs/tutorial/part-07-going-global.md`
- `CLAUDE.md` — Trovato terminology section
