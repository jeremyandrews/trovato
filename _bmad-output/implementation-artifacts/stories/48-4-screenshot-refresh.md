# Story 48.4: Screenshot Refresh

Status: ready-for-dev

## Story

As a tutorial reader,
I want screenshots to match the current UI,
so that I can visually follow along without confusion caused by outdated interface captures.

## Acceptance Criteria

1. All screenshots in `docs/tutorial/images/part-{01-07}/` are verified against the current UI.
2. Mismatched screenshots are recaptured using `screenshot.mjs`.
3. New screenshots are created for UI elements added by Epics A-H that are referenced in the tutorial.
4. Dimensions and format are consistent across all screenshots (thumbnail format per commit `e0fcef9`).
5. Screenshots use the tutorial standard database state (restored from the appropriate backup).
6. No screenshots reference or depict features that do not exist yet in the current kernel.

## Tasks / Subtasks

- [ ] Restore database to tutorial standard state from backup (AC: #5)
- [ ] Start Trovato server and inventory all existing screenshots in `docs/tutorial/images/part-*/` (AC: #1)
- [ ] Compare each screenshot against the current UI, cataloguing matches and mismatches (AC: #1)
- [ ] Recapture all mismatched screenshots using `screenshot.mjs` (AC: #2)
- [ ] Identify UI elements from Epics A-H that are referenced in tutorial text but lack screenshots (AC: #3)
- [ ] Capture new screenshots for newly referenced UI elements (AC: #3)
- [ ] Verify all screenshots use thumbnail format consistent with commit `e0fcef9` conventions (AC: #4)
- [ ] Audit for screenshots depicting features not yet implemented and remove or replace them (AC: #6)
- [ ] Verify all `![alt](path)` references in tutorial markdown resolve to existing image files (AC: #1)

## Dev Notes

### Architecture

Screenshot capture uses `screenshot.mjs` which drives a headless browser. The tutorial standard database state must be used so screenshots show realistic, consistent data. The thumbnail format established in commit `e0fcef9` should be followed for all new and recaptured screenshots.

Workflow per screenshot:
1. Navigate to the relevant page in the running Trovato instance.
2. Capture using `screenshot.mjs` with appropriate viewport/selector options.
3. Verify the output matches thumbnail format conventions.
4. Replace the old file at the same path (so markdown references remain valid).

### Testing

- After all captures, verify every `![...](...)` image reference in Parts 1-7 resolves to an existing file.
- Open each screenshot and visually confirm it matches the current UI.
- Verify file sizes are reasonable (thumbnail format, not full-resolution).

### References

- `docs/tutorial/images/part-01/` through `part-07/`
- `screenshot.mjs`
- Commit `e0fcef9` — thumbnail format convention
- `CLAUDE.md` — Database Backups section
