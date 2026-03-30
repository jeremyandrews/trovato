# Story 49.3: Cross-Reference Audit and Appendix Cleanup

Status: ready-for-dev

## Story

As a reader navigating between design docs,
I want all cross-references to be correct and appendices up to date,
so that I can follow links confidently and find accurate information about deferred issues and terminology.

## Acceptance Criteria

1. All 17 design docs are audited for internal cross-references (wikilinks, markdown links, section references).
2. Broken links are fixed (pointing to renamed or moved documents).
3. Stale references are updated — language like "planned" or "will be implemented" changed to "implemented" where the feature now exists.
4. `Appendix-Deferred-Issues.md` is updated: resolved items are cleared (moved to a "Resolved" section or removed with a note), new deferred items from epic "What's Deferred" sections are added.
5. `Terminology.md` is updated with new terms introduced by Epics A-H.
6. No design doc references a feature that does not exist in the current codebase.
7. Epic docs (`docs/ritrovo/epic-*.md`) cross-reference design docs correctly.

## Tasks / Subtasks

- [ ] Inventory all 17 design docs in `docs/design/` and all epic docs in `docs/ritrovo/` (AC: #1, #7)
- [ ] Extract every cross-reference (wikilinks, relative links, section anchors) from each design doc (AC: #1)
- [ ] Verify each cross-reference resolves to an existing document and section (AC: #1, #2)
- [ ] Fix broken links — update paths for renamed/moved documents (AC: #2)
- [ ] Search for "planned", "will be", "not yet", "future" language and update to reflect current implementation status (AC: #3)
- [ ] Review `Appendix-Deferred-Issues.md`: identify resolved items by checking against current codebase (AC: #4)
- [ ] Clear resolved items from deferred issues (move to "Resolved" section or remove with changelog note) (AC: #4)
- [ ] Collect deferred items from each epic doc's "What's Deferred" section and add to appendix (AC: #4)
- [ ] Review `Terminology.md` against Epics A-H for new terms; add missing entries (AC: #5)
- [ ] Audit design docs for references to unimplemented features; remove or flag as future work (AC: #6)
- [ ] Verify epic doc cross-references to design docs are correct (AC: #7)

## Dev Notes

### Architecture

This is a systematic documentation audit. The most efficient approach:

1. **Build a link map.** Extract all cross-references from all docs into a table: source file, link text, target path, exists (yes/no).
2. **Fix broken links.** Work through the "no" entries.
3. **Tense audit.** Grep for future-tense markers and update based on codebase state.
4. **Appendix updates.** This requires reading each epic doc's deferred section and cross-referencing against the appendix.
5. **Terminology.** Diff the current `Terminology.md` entries against terms used in Epics A-H docs.

Use `grep -r` patterns to find cross-references systematically rather than reading every document line by line.

### Testing

- After all fixes, re-run the link extraction and verify 100% resolution rate.
- Spot-check 5-10 "planned to implemented" tense changes against the actual codebase to confirm the feature exists.
- Verify `Terminology.md` entries are alphabetically ordered and consistently formatted.
- Verify `Appendix-Deferred-Issues.md` has no items that are clearly implemented in the current codebase.

### References

- `docs/design/*.md` — all 17 design documents
- `docs/design/Appendix-Deferred-Issues.md`
- `docs/design/Terminology.md`
- `docs/ritrovo/epic-*.md` — epic documentation
