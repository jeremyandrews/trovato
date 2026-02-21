# Story 28.3: Automatic Path Alias Generation (Pathauto)

Status: review

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

- [x] Define pattern token system with `[title]`, `[type]`, `[yyyy]`, `[mm]`, `[dd]` (AC: #1, #4)
- [x] Implement slug generation (transliterate, lowercase, hyphenate) (AC: #3)
- [x] Integrate auto-generation into item create/update pipeline (AC: #2)
- [x] Handle duplicate alias resolution with numeric suffixes (AC: #5)
- [x] Respect manual alias overrides (AC: #6)
- [x] Store patterns in site config (AC: #7)
- [x] Write unit and integration tests

## Dev Notes

### Dependencies

- URL alias table and middleware already exist from Epic 15 (Story 15.5)
- `UrlAlias` model in `crates/kernel/src/models/url_alias.rs`
- Item create/update in `crates/kernel/src/services/item_service.rs`

### Key Files

- `crates/kernel/src/services/pathauto.rs` — pathauto service
- `crates/kernel/src/models/url_alias.rs` — alias CRUD
- `crates/kernel/src/routes/item.rs` — integrated into create/update handlers
- `crates/kernel/src/routes/admin_content.rs` — integrated into admin create/update

### Code Review Fixes Applied

- **Pathauto on update** — added `update_alias_item()` function; called from both `item.rs` and `admin_content.rs` update handlers
- **Admin content integration** — `auto_alias_item()` now called on admin content create
- **Query optimization** — `generate_unique_alias` uses single `LIKE` query instead of up to 100 sequential lookups
- **Kernel minimality** — noted for future extraction to plugin (architectural change deferred)

## Dev Agent Record

### Implementation Plan

All implementation was completed in a prior session. This session verified each AC against the codebase and added integration tests.

### Completion Notes

- **AC #1**: `get_pattern()` retrieves patterns from `SiteConfig::get(pool, "pathauto_patterns")` as JSON object mapping content types to patterns
- **AC #2**: `auto_alias_item()` called on item create in both `item.rs` and `admin_content.rs`; `update_alias_item()` called on update in both
- **AC #3**: `slugify()` converts to lowercase, replaces non-alphanumeric with hyphens, collapses consecutive hyphens, truncates to 128 chars
- **AC #4**: `expand_pattern()` replaces `[yyyy]`, `[mm]`, `[dd]` tokens from item created timestamp
- **AC #5**: `generate_unique_alias()` uses single LIKE query, tries -1 through -99 suffixes, falls back to UUID fragment
- **AC #6**: `auto_alias_item()` checks `find_by_source()` and returns None if any alias exists
- **AC #7**: Patterns stored in `site_config` table with key `pathauto_patterns`
- **Unit tests**: 10 tests in pathauto.rs covering slugify (6) and expand_pattern (4)
- **Integration tests**: 3 tests added covering alias generation, manual alias preservation, and no-pattern handling
- All 653 unit tests pass, clippy clean, fmt clean

## File List

- `crates/kernel/src/services/pathauto.rs` — pathauto service with slugify, expand_pattern, generate_unique_alias, auto_alias_item, update_alias_item
- `crates/kernel/src/routes/item.rs` — pathauto integration on create/update
- `crates/kernel/src/routes/admin_content.rs` — pathauto integration on admin create/update
- `crates/kernel/tests/integration_test.rs` — 3 new pathauto integration tests

## Change Log

- 2026-02-21: Story implementation verified, integration tests added, story marked for review
