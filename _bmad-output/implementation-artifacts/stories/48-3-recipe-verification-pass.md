# Story 48.3: Recipe Verification Pass

Status: ready-for-dev

## Story

As an agent following a recipe,
I want all recipes tested against the updated kernel,
so that I can execute each recipe end-to-end without encountering broken commands, missing files, or stale UI paths.

## Acceptance Criteria

1. Running `sync-check.sh` produces all-matching hashes (no out-of-sync recipes).
2. Each recipe's command sequences are verified against a fresh Trovato install (restored from backup).
3. Recipe steps referencing config fixtures are verified against actual fixture files on disk.
4. Recipe steps referencing admin UI paths are verified against current route definitions.
5. Any failing recipe steps caused by recipe bugs are fixed in the recipe file.
6. Any failing recipe steps caused by kernel bugs are noted with a `<!-- KERNEL-BUG: description -->` comment and a separate issue filed.
7. All sync hashes are updated after modifications.

## Tasks / Subtasks

- [ ] Restore database from clean backup to establish fresh starting state (AC: #2)
- [ ] Run `bash docs/tutorial/recipes/sync-check.sh` and record initial hash status (AC: #1)
- [ ] For each recipe (Parts 1-7), execute command sequences step-by-step against fresh install (AC: #2)
- [ ] Verify all config fixture references (`config/*.yaml`, `config/*.toml`, etc.) match actual files (AC: #3)
- [ ] Verify all admin UI path references (`/admin/*`) match current route definitions in `routes/` (AC: #4)
- [ ] Fix recipe bugs found during verification (AC: #5)
- [ ] Document kernel bugs with `<!-- KERNEL-BUG -->` comments and file separate issues (AC: #6)
- [ ] Update all sync hashes in `sync-check.sh` after modifications (AC: #7)
- [ ] Run `sync-check.sh` a final time to confirm all hashes match (AC: #1)

## Dev Notes

### Architecture

Recipes are the agent-facing counterpart to the human-facing tutorial. They must be mechanically executable — every command must work, every path must resolve, every expected output must match. This story is purely verification and fixing; no new recipe content is added here (that happens in 48.1 and 48.2 if tutorial content changes require recipe updates).

Start from a fresh database state using the backup/restore procedure documented in CLAUDE.md. Work through recipes sequentially (Part 1 first) since later recipes may depend on state created by earlier ones.

### Testing

- Execute each recipe in a clean environment, recording pass/fail per step.
- After fixing all recipe bugs, do a complete sequential run of all recipes from Part 1 through Part 7 to confirm no regressions.
- Final `sync-check.sh` must show all hashes matching.

### References

- `docs/tutorial/recipes/recipe-part-01.md` through `recipe-part-07.md`
- `docs/tutorial/recipes/sync-check.sh`
- `CLAUDE.md` — Database Backups section, Working Through the Tutorial section
