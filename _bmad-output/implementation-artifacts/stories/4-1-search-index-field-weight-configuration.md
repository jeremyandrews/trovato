# Story 4.1: Search Index Field Weight Configuration

Status: ready-for-dev

## Story

As a site administrator,
I want search field weights configured so that title matches rank higher than description matches,
So that search results are ordered by relevance.

## Acceptance Criteria

1. Search field weight config imported: title=A, description=B, city/country=C
2. Search index rebuilt with configured weights
3. Title matches rank above description-only matches

## Tasks / Subtasks

- [ ] Create `variable.search_field_config.yml` with field weights (AC: #1)
- [ ] Import config and rebuild search index (AC: #2)
- [ ] Verify weighting: search for term in title vs description (AC: #3)

## Dev Notes

- Search service: `crates/kernel/src/search/mod.rs`, `routes/search.rs`
- PostgreSQL tsvector with setweight() for A/B/C/D weights
- Config-driven: field weights stored in variable config
- Index rebuild: CLI command or programmatic trigger

### References

- [Source: docs/design/search-architecture.md] — search design
- [Source: docs/tutorial/plan-parts-03-04.md#Step 6] — search configuration
