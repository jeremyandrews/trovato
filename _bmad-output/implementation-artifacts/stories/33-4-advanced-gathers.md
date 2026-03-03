# Story 33.4: Advanced Gathers with Exposed & Contextual Filters

Status: not started

## Story

As a **Ritrovo visitor**,
I want to filter conferences by topic, country, online-only, and CFP deadline from the listing page,
so that I can find events relevant to my interests without scrolling through hundreds of results.

## Acceptance Criteria

1. "Upcoming Conferences" Gather at `/conferences` upgraded with four exposed filters:
   - Topic: `field_topics InCategory` (hierarchical — selecting "Languages" includes all sublanguages)
   - Country: `field_country equals` (exact match, case-insensitive)
   - Online only: `field_online equals true`
   - Language: `field_language equals`
2. Exposed filter form renders above the results; filters compose as AND
3. Pagination works correctly with active filters (page 2 of filtered results)
4. Filter state preserved in URL query string (bookmarkable: `/conferences?topic=rust&country=Germany`)
5. "Open CFPs" Gather at `/cfps`:
   - Filter: `field_cfp_end_date >= today` AND `field_cfp_url IS NOT NULL`
   - Sort: `field_cfp_end_date` ascending (nearest deadline first)
   - 20 items per page
6. "By Topic" Gather at `/topics/{slug}`:
   - Contextual filter: topic term ID from URL argument
   - `InCategory` traversal (clicking "Languages" shows all language conferences)
   - Filter: `field_start_date >= today`
7. "By Location" Gathers at `/location/{country}` and `/location/{country}/{city}`:
   - Country-level: filter `field_country = {url arg 0}`
   - City-level: filter `field_country = {url arg 0}` AND `field_city = {url arg 1}`
   - Filter: `field_start_date >= today`
8. Config YAML for all four Gathers in `docs/tutorial/config/`
9. Tutorial section covers: exposed vs contextual filters, URL arg pattern, AND composition
10. `trovato-test` blocks assert: exposed filter narrows results, contextual filter uses URL arg, pagination with filter, CFP deadline filter

## Tasks / Subtasks

- [ ] Upgrade "Upcoming Conferences" Gather with exposed filters (AC: #1, #2, #3, #4)
  - [ ] Add `field_topics InCategory` exposed filter with `exposed_label: "Topic"`
  - [ ] Add `field_country equals` exposed filter with `exposed_label: "Country"`
  - [ ] Add `field_online equals` exposed filter (boolean, checkbox in UI)
  - [ ] Add `field_language equals` exposed filter with `exposed_label: "Language"`
  - [ ] Verify exposed filter form renders in `query--upcoming_conferences.html` template
  - [ ] Verify filter values preserved in pager links
- [ ] Implement date comparison fix for `field_start_date >= today` (AC: #6, #7)
  - [ ] The Part 1 story (29.3) deferred this — ISO date strings in JSONB can't use `as_i64()`
  - [ ] Add `CurrentDate` contextual value that returns ISO date string `YYYY-MM-DD`
  - [ ] Add string-comparison path to `GreaterOrEqual`/`GreaterThan` operator for date-format values
  - [ ] Or add a dedicated `DateGreaterOrEqual` operator
- [ ] Create "Open CFPs" Gather (AC: #5)
  - [ ] `field_cfp_end_date >= today` (uses date fix above) AND `field_cfp_url is_not_null`
  - [ ] Sort by `field_cfp_end_date` ASC
  - [ ] URL alias: `/cfps` → `/gather/open_cfps`
  - [ ] Config YAML: `docs/tutorial/config/gather.open_cfps.yml`
- [ ] Create "By Topic" Gather (AC: #6)
  - [ ] Contextual filter: `field_topics has_tag_or_descendants {url_arg: "topic"}`
  - [ ] URL alias per topic term (from Story 33.3) or wildcard route
  - [ ] Template: extends `query--upcoming_conferences.html` or reuses it
  - [ ] Config YAML: `docs/tutorial/config/gather.by_topic.yml`
- [ ] Create "By Location" Gather (AC: #7)
  - [ ] Country-level gather `by_country` with contextual filter `field_country = {url_arg: "country"}`
  - [ ] City-level extend with second contextual filter `field_city = {url_arg: "city"}`
  - [ ] URL aliases: `/location/{country}` and `/location/{country}/{city}`
  - [ ] Config YAML: `docs/tutorial/config/gather.by_location.yml`
- [ ] Write tutorial section 2.4 (AC: #9)
- [ ] Write `trovato-test` blocks (AC: #10)

## Dev Notes

### Date Comparison Fix (Blocker for Several ACs)

The `GreaterOrEqual` operator in `gather/query_builder.rs` calls `filter.value.as_i64()`. ISO dates stored as JSONB strings compare lexicographically (which works correctly for YYYY-MM-DD format), but the current value path doesn't support string contextual values.

Proposed solution — add a `CurrentDate` contextual filter value variant:

```rust
// In gather/types.rs FilterValue
Contextual(String),  // already exists — "current_user", add "current_date"
```

In `query_builder.rs`, resolve `"current_date"` to `chrono::Local::now().format("%Y-%m-%d").to_string()` and emit a string comparison:

```sql
item.fields->>'field_cfp_end_date' >= '2026-03-03'
```

This is lexicographic comparison — valid for YYYY-MM-DD formatted strings.

### Exposed Filter URL Preservation in Pager

The pager template (`gather/pager.html`) must include active filter values in page links. Verify the pager builds URLs like `/conferences?topic=rust&country=Germany&page=2`. If it doesn't, fix the pager template and the `render_gather_with_theme` context injection.

### Country/City URL Patterns

`/location/{country}` and `/location/{country}/{city}` cannot be static URL aliases — they're parameterised. Options:
1. Register Axum routes for these patterns in the plugin via a route tap
2. Create a catchall alias with URL arg extraction in the fallback handler

Option 1 is cleaner. A plugin tap `tap_routes` (if it exists) can add routes. If not, these can be kernel routes gated by `plugin_gate!("ritrovo_importer")`.

### Contextual Filter URL Args

The gather `url_args` map in `QueryContext` is populated from query string parameters in `render_query_html`. Contextual filters that read from `url_args` should declare `{"Contextual": "country"}` as their value, and the query builder resolves it from `context.url_args["country"]`.

For path-based args (e.g. `/location/Germany`), the route handler must extract the path segment and inject it into the query context. This is the main implementation complexity for the location Gather.

### `InCategory` Exposed Filter UI

The exposed filter form for `field_topics InCategory` should ideally render as a dropdown populated with term labels. The base gather filter form renders all exposed filters as text inputs. For Part 2, a text input accepting the topic slug is acceptable; a dropdown comes in Part 3 with the theming work.

### Config YAML Format

Follow the pattern from `docs/tutorial/config/variable.pathauto_patterns.yml`. Each Gather needs a YAML file serializing the full `GatherQuery` struct (query_id, label, description, definition, display). Check the existing format via `cargo run --bin trovato -- config export`.

### Key Files

- `crates/kernel/src/gather/types.rs` — add `CurrentDate` contextual value
- `crates/kernel/src/gather/query_builder.rs` — string comparison for date fields
- `templates/gather/query--upcoming_conferences.html` — add exposed filter form rendering
- `templates/gather/pager.html` — preserve filter params in page links
- `docs/tutorial/config/gather.*.yml` — four config files
- `docs/tutorial/part-02-ritrovo-importer.md` — section 2.4

### Dependencies

- Story 33.3 complete (`has_tag_or_descendants` working, `field_topics` populated)
- Date comparison fix must be implemented (blocks open_cfps and by_topic date filters)
- Gather exposed filter form infrastructure already exists in `query.html`

### References

- Gather filter types: `crates/kernel/src/gather/types.rs`
- Query builder: `crates/kernel/src/gather/query_builder.rs`
- Existing exposed filter form: `templates/gather/query.html`
- Pager template: `templates/gather/pager.html`
- Story 29.3 dev notes on date filter limitation

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
