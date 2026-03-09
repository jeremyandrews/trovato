# Story 2.3: Speaker Pathauto URL Aliases

Status: ready-for-dev

## Story

As a site visitor,
I want speaker pages to have clean URLs like `/speakers/{name}`,
So that speaker URLs are readable and shareable.

## Acceptance Criteria

1. Pathauto pattern `/speakers/[item:title]` configured for speaker type
2. New speaker "Jane Doe" gets alias `/speakers/jane-doe`
3. Duplicate names deduplicated (e.g., `/speakers/jane-doe-1`)
4. Name updates regenerate the URL alias

## Tasks / Subtasks

- [ ] Update `variable.pathauto_patterns.yml` with speaker pattern (AC: #1)
- [ ] Re-import config
- [ ] Verify alias generation for new speaker (AC: #2)
- [ ] Verify deduplication with same-name speaker (AC: #3)
- [ ] Verify alias regeneration on title update (AC: #4)

## Dev Notes

### Architecture

- Pathauto service: `crates/kernel/src/pathauto/` -- `generate_unique_alias()` prepends `/`
- URL alias table: `url_alias` with `created` as `bigint` (unix timestamp, NOT timestamp type)
- Pattern format: `speakers/[item:title]` -- title gets slugified automatically
- Deduplication: pathauto appends `-1`, `-2` etc. for conflicts
- Alias regeneration triggers on item save when title changes

### Key Files

- Config: `variable.pathauto_patterns.yml` -- add `speaker: speakers/[title]` pattern
- `crates/kernel/src/pathauto/` -- pattern application and alias generation
- `url_alias` table -- stores source path -> alias mappings

### References

- [Source: crates/kernel/src/pathauto/] -- pathauto implementation
- [Source: docs/tutorial/plan-parts-03-04.md#Step 3] -- speaker pathauto setup
