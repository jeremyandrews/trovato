# Story 28.3: Automatic Path Alias Generation (Pathauto)

Status: ready-for-dev

## Story

As a **content editor**,
I want URL aliases automatically generated from configurable patterns,
So that every item gets a human-readable URL without manual entry.

## Acceptance Criteria

1. Pattern configuration per content type (e.g., `blog/[title]`, `news/[yyyy]/[mm]/[title]`)
2. Aliases auto-generated on item create and update (unless manually overridden)
3. Title tokens sanitized to URL-safe slugs (lowercase, hyphens, no special chars)
4. Date tokens supported: `[yyyy]`, `[mm]`, `[dd]`
5. Duplicate aliases get numeric suffixes (e.g., `blog/my-post-1`)
6. Existing manual aliases are not overwritten
7. Pattern management via site config (no admin UI needed for v1.0)

## Tasks / Subtasks

- [ ] Define pattern token system with `[title]`, `[type]`, `[yyyy]`, `[mm]`, `[dd]` (AC: #1, #4)
- [ ] Implement slug generation (transliterate, lowercase, hyphenate) (AC: #3)
- [ ] Integrate auto-generation into item create/update pipeline (AC: #2)
- [ ] Handle duplicate alias resolution with numeric suffixes (AC: #5)
- [ ] Respect manual alias overrides (AC: #6)
- [ ] Store patterns in site config (AC: #7)
- [ ] Write unit and integration tests

## Dev Notes

### Dependencies

- URL alias table and middleware already exist from Epic 15 (Story 15.5)
- `UrlAlias` model in `crates/kernel/src/models/url_alias.rs`
- Item create/update in `crates/kernel/src/services/item_service.rs`

### Key Files

- `crates/kernel/src/services/` — new pathauto service or add to item_service
- `crates/kernel/src/models/url_alias.rs` — alias CRUD
- `crates/kernel/src/routes/item.rs` — integrate into create/update handlers
