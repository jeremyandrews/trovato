# Story 34.3: Dynamic Options Widget (Dropdown / Autocomplete)

Status: ready-for-dev

## Story

As a **Ritrovo visitor**,
I want the "Country" and "Language" filters to show a dropdown of real values that exist in the database,
so that I can pick exactly what's there instead of guessing whether to type "usa", "u.s.a.", or "United States".

## Acceptance Criteria

1. `ExposedWidget` gains a `DynamicOptions` variant; JSON: `"widget": {"type": "dynamic_options", "source_field": "fields.field_country", "autocomplete_threshold": 30}`
2. `autocomplete_threshold` is optional in JSON; defaults to 30 when omitted
3. At render time, the kernel fetches `DISTINCT` non-null, non-empty values for `source_field` from the `item` table, filtered to the item type of the gather query and `status = 1`
4. When distinct count ≤ threshold: renders a `<select>` with "Any" (blank) first, then sorted options
5. When distinct count > threshold: renders `<input type="text" list="{field}-options">` + `<datalist id="{field}-options">` with all values (browser handles client-side prefix filtering)
6. Submitted value is matched with a case-insensitive `Equals` at SQL level (operator in the filter definition remains `Equals`; the widget only affects rendering)
7. Selected value preserved on re-render for both widget variants
8. Options are cached per `(source_field, item_type)` with a 300-second TTL; cache hit skips the DB query
9. Distinct value query excludes NULL and `''`; values are sorted alphabetically
10. Ritrovo `field_country` and `field_language` filters updated to `dynamic_options` widget; threshold 30 for both
11. Tests: below-threshold renders `<select>`, above-threshold renders `<datalist>`, null/empty values excluded, cache hit avoids second DB call

## Tasks / Subtasks

- [ ] Add `DynamicOptions { source_field: String, autocomplete_threshold: usize }` to `ExposedWidget` (AC: #1, #2)
  - [ ] `#[serde(default = "default_autocomplete_threshold")]` on `autocomplete_threshold`; default value 30
  - [ ] `source_field` validated with `is_valid_field_name` before SQL use (AC: #3)
- [ ] Implement `fetch_distinct_values(source_field: &str, item_type: &str, pool: &PgPool) -> Result<Vec<String>>` (AC: #3, #9)
  - [ ] For JSONB fields (`fields.field_country` → `fields->>'field_country'`): extract the key suffix and build: `SELECT DISTINCT fields->>$1 FROM item WHERE type = $2 AND status = 1 AND fields->>$1 IS NOT NULL AND fields->>$1 != '' ORDER BY 1`
  - [ ] For top-level columns (`title`, `type`): use column directly
  - [ ] Validate the field name with `is_valid_field_name` before interpolation (SQL injection prevention)
- [ ] Add options cache to `GatherService` or route-level cache: `moka::sync::Cache<(String, String), Vec<String>>` with TTL 300s (AC: #8)
- [ ] Update `render_exposed_filter_form` to handle `DynamicOptions` (AC: #4, #5, #7)
  - [ ] Receive pre-fetched options as a `HashMap<String, Vec<String>>` (keyed by field name) — same pre-fetch pattern as Story 34.2
  - [ ] Branch on count vs threshold; render `<select>` or `<datalist>`
- [ ] Update ritrovo_importer: add `dynamic_options` widget to `field_country` and `field_language` filter definitions (AC: #10)
- [ ] Write tests (AC: #11)

## Dev Notes

- **SQL injection prevention**: `source_field` in the gather JSON is plugin-supplied config, not user input — but apply `is_valid_field_name` anyway as defence-in-depth. For JSONB access, parse `fields.field_country` → `("fields", "field_country")` and build `fields->>$1` with the key as a bind parameter, not interpolated.
- **Datalist vs custom autocomplete**: Using `<datalist>` keeps this story completely server-side and framework-free. The browser renders a native suggestion dropdown as the user types. This is sufficient for O(100s) of values. A future story can upgrade to a JS autocomplete widget if needed.
- **Cache placement**: A simple `OnceLock<moka::sync::Cache<...>>` in `routes/gather.rs` is fine if the cache is route-local. Alternatively, add a `distinct_values_cache` field to `GatherService` (already holds other caches). Pick whichever is simpler given the current `AppState` structure.
- **Pre-fetch pattern** (same as Story 34.2): gather route handler fetches options before calling `render_exposed_filter_form`, passes results as `HashMap<String, Vec<String>>` keyed by field name. This keeps the render function sync and easily unit-testable with mock data.
- **`source_field` format**: the field path in the filter JSON uses dot notation (`fields.field_country`). `fetch_distinct_values` must interpret this consistently with how `query_builder.rs` resolves the same path.
- **Case-insensitive match**: operator `Contains` (ILIKE) already gives case-insensitive matching for country/language. If the operator is `Equals`, use `LOWER()` or `ILIKE` without wildcards. Document the expected operator in the gather query config.
- **Empty option list**: if the gather has no results yet (fresh install), render the select with only "Any" rather than panicking or showing an error.

### Project Structure Notes

- `crates/kernel/src/gather/types.rs` — `ExposedWidget::DynamicOptions`, `default_autocomplete_threshold`
- `crates/kernel/src/routes/gather.rs` — `render_exposed_filter_form`, `fetch_distinct_values` helper, optional options cache
- `crates/kernel/src/gather/gather_service.rs` — alternative cache location
- `crates/kernel/src/routes/helpers.rs` — `is_valid_field_name` for field path validation
- `plugins/ritrovo_importer/src/lib.rs` — `field_country` and `field_language` filter JSON

### References

- [Source: crates/kernel/src/gather/types.rs] — `ExposedWidget` (after Stories 34.1–34.2)
- [Source: crates/kernel/src/routes/gather.rs] — render function and fetch helper location
- [Source: crates/kernel/src/routes/helpers.rs] — `is_valid_field_name` for SQL safety
- [Source: docs/coding-standards.md] — SQL injection rules: validate with `is_valid_field_name` before interpolation
- [Source: CLAUDE.md#SQL Injection Prevention] — SeaQuery/parameterized query requirement; `is_valid_field_name` for JSONB paths

## Dev Agent Record

### Agent Model Used

claude-sonnet-4-6

### Debug Log References

### Completion Notes List

### File List
