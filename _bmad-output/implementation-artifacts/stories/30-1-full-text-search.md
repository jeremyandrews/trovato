# Story 30.1: Full-Text Search

Status: not started

## Story

As a **Ritrovo visitor**,
I want to search conferences by keyword from any page,
so that I can find relevant events without knowing how to navigate the topic hierarchy.

## Acceptance Criteria

1. PostgreSQL `tsvector` index built from conference fields with field weighting:
   - Weight A (highest): `title`
   - Weight B: `field_description`
   - Weight C: `field_city`, topic names (denormalized at index time)
2. Search index updated automatically when a conference item is created or updated
3. Search box present in site header on all pages
4. Search results page at `/search?q=` returns results ranked by `ts_rank`
5. Each result shows a snippet with matched terms highlighted via `ts_headline`
6. Empty query (`/search?q=`) handled gracefully — shows prompt, not an error or empty list
7. Searching a term with no matches shows "No results found for {term}" message
8. Only items in the live stage are indexed (stage-scoped indexing for other stages deferred to Part 4)
9. Tutorial section covers: tsvector/tsquery, field weighting rationale, foreshadows AI semantic search in Part 9
10. `trovato-test` blocks assert: title match ranks above description match, city match returns result, empty query handled, live-stage-only results

## Tasks / Subtasks

- [ ] Configure search index for `conference` item type (AC: #1)
  - [ ] Add `SearchFieldConfig` entries for title (A), field_description (B), field_city (C), topic names (C)
  - [ ] Topic names require denormalization: on index, resolve `field_topics` term IDs to term labels
  - [ ] Config YAML: `docs/tutorial/config/search.conference.yml`
- [ ] Verify search index trigger fires on conference create/update (AC: #2)
  - [ ] `20260213000002_create_search_trigger.sql` should handle this generically
  - [ ] Confirm JSONB field extraction works for `field_description`, `field_city`
  - [ ] Add topic name denormalization to the trigger or update path
- [ ] Add search box to site header (AC: #3)
  - [ ] Edit `templates/base.html` (or header partial) to include search form
  - [ ] Form GETs `/search?q={query}`
- [ ] Implement `/search` route and results template (AC: #4, #5, #6, #7)
  - [ ] Route handler in `crates/kernel/src/routes/` (or plugin route if search is plugin-gated)
  - [ ] SQL: `SELECT ..., ts_rank(search_vector, query) AS rank, ts_headline(...) AS snippet FROM item WHERE search_vector @@ query ORDER BY rank DESC`
  - [ ] Template: `templates/search/results.html`
  - [ ] Empty query renders `templates/search/empty.html` with prompt
  - [ ] No-results case renders appropriate message
- [ ] Scope index to live stage only (AC: #8)
  - [ ] Filter: `WHERE stage_id = {LIVE_STAGE_ID}`
  - [ ] Note in tutorial: stage-scoped indexing added in Part 4
- [ ] Write tutorial section 2.5 (AC: #9)
  - [ ] Explain tsvector/tsquery and why PostgreSQL FTS is a good fit
  - [ ] Field weighting rationale
  - [ ] Foreshadow Part 9 AI semantic search ("find conferences about reliable distributed systems")
- [ ] Write `trovato-test` blocks (AC: #10)

## Dev Notes

### Existing Search Infrastructure

A search system already exists. Check:
- `crates/kernel/migrations/20260213000001_create_search_config.sql` — search config table
- `crates/kernel/migrations/20260213000002_create_search_trigger.sql` — auto-update trigger
- `crates/kernel/src/search/` — search service
- `plugins/trovato_search/` — search plugin (if enabled, may already provide `/search` route)

**Before implementing anything**, read these files to understand what already exists. The work may be mostly configuration rather than new code.

### Search Field Config

The `search_field_config` table (from the migration) likely has columns: `bundle` (item type), `field_name`, `weight`. Insert rows for `conference`:

```sql
INSERT INTO search_field_config (bundle, field_name, weight) VALUES
  ('conference', 'title', 'A'),
  ('conference', 'field_description', 'B'),
  ('conference', 'field_city', 'C');
```

Topic name denormalization is more complex — the tsvector trigger would need to JOIN `category_tag` to resolve term IDs in `fields->>'field_topics'` to labels. Implement this only if the existing trigger is extensible; otherwise document as a Part 3 enhancement.

### `ts_headline` Snippets

```sql
ts_headline(
    'english',
    coalesce(fields->>'field_description', title),
    plainto_tsquery('english', $query),
    'MaxWords=35, MinWords=15, StartSel=<mark>, StopSel=</mark>'
) AS snippet
```

The `<mark>` tags need to pass through Tera's autoescaping — use `| safe` with a `{# SAFE: ts_headline output — postgres escapes HTML in headline context #}` comment, or sanitize before rendering.

### Search Route Placement

If `plugins/trovato_search/` already implements a `/search` route, configure it rather than building a new one. The tutorial should explain how to enable the plugin and configure search fields.

If no search route exists, add one to `crates/kernel/src/routes/` gated by a feature check. Follow the existing pattern in `admin.rs` or `gather.rs`.

### Header Search Box

`templates/base.html` is the site-wide layout. The search form should be minimal:

```html
<form action="/search" method="get" class="site-search">
  <input type="search" name="q" placeholder="Search conferences..."
         value="{{ search_query | default(value='') }}">
  <button type="submit">Search</button>
</form>
```

`search_query` must be injected into the Tera context by `inject_site_context` in `routes/helpers.rs` when the current path is `/search`.

### AI Search Foreshadow Text (for tutorial)

> Keyword search finds conferences when the words you type appear in the title, description, or location. It is fast and deterministic. Part 9 of this tutorial adds a second search mode powered by embeddings — you'll be able to search for "conferences about reliable distributed systems" and find RustConf even if those exact words don't appear in its description. For now, keyword search handles the common case well.

### Key Files

- `crates/kernel/migrations/20260213000001_create_search_config.sql` — read first
- `crates/kernel/migrations/20260213000002_create_search_trigger.sql` — read first
- `crates/kernel/src/search/` — read first
- `plugins/trovato_search/` — read first (may already have `/search` route)
- `templates/base.html` — add search box
- `templates/search/results.html` — new template
- `docs/tutorial/config/search.conference.yml` — config YAML
- `docs/tutorial/part-02-ritrovo-importer.md` — section 2.5

### Dependencies

- Story 33.3 complete (topic names available for denormalization)
- `trovato_search` plugin or equivalent search infrastructure enabled
- Conference items must exist and be indexed (Story 33.2)

### References

- BMAD Epic 2 (`docs/ritrovo/epic-02.md`) — search epic narrative
- PostgreSQL full-text search docs: `https://www.postgresql.org/docs/current/textsearch.html`
- Existing search migration: `crates/kernel/migrations/20260213000001_create_search_config.sql`

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
