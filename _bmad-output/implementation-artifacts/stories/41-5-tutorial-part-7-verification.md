# Story 41.5: Tutorial Part 7 Verification

Status: ready-for-dev

## Story

As a **tutorial reader** working through Part 7 (Going Global),
I want the tutorial content to reflect the current i18n infrastructure accurately,
So that the tutorial teaches real, working features without misleading about what is implemented vs. stubbed.

## Acceptance Criteria

1. Part 7 tutorial verified against actual kernel behavior: every code example runs, every screenshot matches current UI
2. Tutorial notes where the `trovato_locale` plugin is a stub (permissions + menu only, no translation UI) -- sets expectations correctly
3. Tutorial demonstrates `format_date` with locale parameter (new capability from Story 41.2)
4. Tutorial demonstrates `Item.language` visibility to plugins (new capability from Story 41.1)
5. Recipe `recipe-part-07.md` updated to match any tutorial changes
6. Sync hash updated in `docs/tutorial/recipes/sync-check.sh`
7. `trovato-test` blocks in Part 7 pass against updated kernel

## Tasks / Subtasks

- [ ] Read `docs/tutorial/part-07-going-global.md` end-to-end (AC: #1)
- [ ] Run each code block and CLI command against the running system, note failures (AC: #1)
- [ ] Verify each screenshot matches current UI, recapture any that are stale (AC: #1)
- [ ] Verify tutorial clearly notes locale plugin stub status (AC: #2)
- [ ] Add or update section demonstrating `format_date` with `trovato_locale` parameter (AC: #3)
- [ ] Add or update section showing `Item.language` is now visible to plugins (AC: #4)
- [ ] Update `docs/tutorial/recipes/recipe-part-07.md` to match tutorial changes (AC: #5)
- [ ] Run `bash docs/tutorial/recipes/sync-check.sh` and update hashes (AC: #6)
- [ ] Run `trovato-test` blocks from Part 7 and verify all pass (AC: #7)
- [ ] Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`

## Dev Notes

### Architecture

This is a verification and documentation story, not a code change story. It depends on Stories 41.1 and 41.2 being complete first, since the tutorial needs to demonstrate `format_date` with locale and `Item.language` visibility.

### Testing

- All `trovato-test` blocks in Part 7 must pass
- Recipe sync check must pass after updates
- Manual verification of screenshots against running system

### References

- `docs/tutorial/part-07-going-global.md` -- tutorial source
- `docs/tutorial/recipes/recipe-part-07.md` -- agent recipe
- `docs/tutorial/recipes/sync-check.sh` -- sync verification script
- Depends on: Story 41.1 (Item.language), Story 41.2 (format_date locale)
- [Epic 41 source: docs/ritrovo/epic-11-i18n.md]
