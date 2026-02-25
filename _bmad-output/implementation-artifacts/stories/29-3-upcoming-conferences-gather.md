# Story 29.3: "Upcoming Conferences" Gather with Pagination

Status: done

## Story

As a **site visitor**,
I want to see a list of upcoming conferences sorted by date,
so that I can discover conferences to attend.

## Acceptance Criteria

1. Gather definition created for `conference` Items with fields: name, start_date, end_date, city, country, online
2. Filter: `status = 1` (published conferences only); dynamic `start_date >= current_date` deferred to Part 2 (gather `GreaterOrEqual` operator requires integer values, not ISO date strings)
3. Sort: `fields.field_start_date` ascending (soonest first)
4. Pagination: 25 items per page with next/previous controls
5. Gather attached to `/conferences` route via URL alias, accessible by anonymous users
6. Default rendering shows field values (name, dates, city, country, online status) -- no custom templates yet
7. Empty state handled gracefully (message when no upcoming conferences exist)
8. Tutorial Step 4 documentation written covering Gather definition, SQL generation, routing, and pagination

## Tasks / Subtasks

- [x] Create the "Upcoming Conferences" Gather definition (AC: #1, #2, #3, #4)
  - [x] Define base_table: "item", item_type: "conference"
  - [x] Add filter: status = 1 (published only)
  - [x] Add sort: fields.field_start_date ASC
  - [x] Configure pager: items_per_page = 25
  - [x] Set stage_aware: true
- [x] Attach Gather to `/conferences` route (AC: #5)
  - [x] Create URL alias: /conferences → /gather/upcoming_conferences
  - [x] Verify anonymous access works (path alias middleware does not require auth)
- [x] Verify default rendering (AC: #6)
  - [x] Table format displays field values
  - [x] Pager shows total count
- [x] Handle empty state (AC: #7)
  - [x] Empty text configured: "No conferences found."
- [x] Write tutorial Step 4 documentation (AC: #8)
  - [x] Gather definition syntax/structure
  - [x] Under the Hood: generated SQL query shown
  - [x] How URL alias attaches Gather to /conferences route
  - [x] Pagination mechanics
  - [x] Admin UI alternative for creating Gathers
  - [x] Exposed filters preview (foreshadowing Part 2)

## Dev Notes

### Dependencies

- Story 29.1 (Define conference Item Type) must be complete
- Story 29.2 (Admin UI) should be complete (need seed data to see results)
- Epic 7 (Gather) provides all Gather infrastructure -- complete
- Epic 23 (Gather UI) provides admin UI for creating Gathers -- complete

### Gather Route Attachment

Gathers are served at `/gather/{query_id}` by default. To attach a Gather to a clean URL like `/conferences`, create a URL alias:

```sql
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (gen_random_uuid(), '/gather/upcoming_conferences', '/conferences', 'en', 'live', ...);
```

The path alias middleware (in `crates/kernel/src/middleware/path_alias.rs`) rewrites `/conferences` to `/gather/upcoming_conferences` transparently. This is the same pattern used by the blog plugin (`/blog` → `/gather/blog_listing`).

### Date Filter Limitation

The epic's AC #2 specifies `start_date >= current_date`. The gather system's `GreaterOrEqual` operator calls `as_i64()` on the filter value, which works for integer comparisons but not for ISO date strings stored in JSONB. The `CurrentTime` contextual value resolves to a Unix timestamp integer, which is incomparable to ISO 8601 date strings like `"2026-09-09"`. This filter is deferred to Part 2, where we can either:
- Add string comparison support to the gather filter operators
- Store dates as Unix timestamps instead of ISO strings
- Add a `CurrentDate` contextual value that returns an ISO date string

For Part 1, all seeded conferences are in the future, so the listing is effectively "upcoming" already.

### Key Files

- ~~`crates/kernel/migrations/20260226000002_seed_conference_gather.sql`~~ -- Gather + URL alias migration (deleted: tutorial now guides users to create these by hand)
- `crates/kernel/src/routes/gather.rs` -- Gather route handlers
- `crates/kernel/src/middleware/path_alias.rs` -- URL alias middleware
- `docs/tutorial/part-01-hello-trovato.md` -- Tutorial chapter

### References

- [Source: docs/ritrovo/overview.md#Gather Definitions]
- [Source: docs/design/Design-Query-Engine.md]
- [Source: docs/ritrovo/epic-01.md#Step 4: Build Your First Gather]
- [Source: plugins/blog/migrations/001_seed_gather_query.sql] -- URL alias pattern

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

### Completion Notes List

- ~~Created kernel migration `20260226000002_seed_conference_gather.sql` following the blog plugin pattern~~ (deleted: replaced by hands-on tutorial + test helper)
- Gather query_id: `upcoming_conferences`, item_type: `conference`, status=1 filter, sort by `fields.field_start_date` ASC, 25 items/page, table format, stage_aware: true
- URL alias: `/conferences` → `/gather/upcoming_conferences` with ON CONFLICT upsert
- Date filter (`start_date >= current_date`) deferred — gather `GreaterOrEqual` operator only accepts integer values via `as_i64()`, incompatible with ISO date strings in JSONB
- Tutorial Step 4 rewritten: covers migration-based Gather creation, URL alias routing mechanism, generated SQL walkthrough, pagination, empty state, admin UI alternative, exposed filters preview
- Tutorial Steps 1-3 also updated to match epic-01.md: 4-step installer, 12-char password minimum, why SQL is safe, config export/import, API verification, item viewing at `/item/{id}`, template resolution order, JSON API, improved stage explanation, "What's Deferred" section, Related links

### File List

- ~~`crates/kernel/migrations/20260226000002_seed_conference_gather.sql`~~ -- Deleted: gather + URL alias migration replaced by hands-on tutorial + test helper
- `docs/tutorial/part-01-hello-trovato.md` -- Rewritten: all 4 steps aligned with epic-01.md
