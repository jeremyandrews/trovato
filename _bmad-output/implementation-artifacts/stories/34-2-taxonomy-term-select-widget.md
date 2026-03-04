# Story 34.2: Taxonomy Term Select Widget

Status: ready-for-dev

## Story

As a **Ritrovo visitor**,
I want the "Topic" filter to show a human-readable dropdown of taxonomy terms instead of a UUID input,
so that I can filter conferences by topic without looking up term IDs.

## Acceptance Criteria

1. `ExposedWidget` (from Story 34.1) gains a `TaxonomySelect` variant carrying the vocabulary machine name; JSON: `"widget": {"type": "taxonomy_select", "vocabulary": "topic"}`
2. At render time, all terms from the specified vocabulary are loaded via `CategoryService`
3. Terms render as a `<select>` with "Any" (blank value) first, then terms sorted by hierarchy depth and name, indented with `&nbsp;&nbsp;` per depth level:
   ```html
   <select name="{field}">
     <option value="">Any</option>
     <option value="42">Languages</option>
     <option value="43">&nbsp;&nbsp;Systems</option>
     <option value="44">&nbsp;&nbsp;&nbsp;&nbsp;Rust</option>
   </select>
   ```
4. Submitted value is the term ID (integer string); the query executes with the `HasTagOrDescendants` operator so selecting a parent returns conferences under all subtopics
5. Selected term is pre-selected on re-render
6. If the vocabulary does not exist or has no terms, the select renders with only "Any" (no error)
7. Ritrovo `field_topics` filter updated to `"widget": {"type": "taxonomy_select", "vocabulary": "topic"}`
8. Tests: term select renders with correct hierarchy indentation, selected value preserved, empty vocabulary renders safely

## Tasks / Subtasks

- [ ] Add `TaxonomySelect { vocabulary: String }` variant to `ExposedWidget` in `types.rs` (AC: #1)
  - [ ] Serde: use `#[serde(tag = "type", rename_all = "snake_case")]` on `ExposedWidget` so struct variants serialize as `{"type": "taxonomy_select", "vocabulary": "..."}`; update `Boolean` to serialize as `{"type": "boolean"}` (or keep as plain string via a custom impl — pick the simpler path)
  - [ ] Ensure backward compatibility: plain `"boolean"` string must still deserialize after serde tag change, OR keep `Boolean` as a unit variant deserializable both ways
- [ ] Update `render_exposed_filter_form` signature to accept `&AppState` (needed for async DB access) (AC: #2)
  - [ ] Caller in `routes/gather.rs` already has `State<Arc<AppState>>`; thread it through
  - [ ] For `TaxonomySelect`: call `CategoryService::get_terms_for_vocabulary(vocabulary)` (or equivalent)
- [ ] Implement `render_taxonomy_select` helper: load terms, sort by depth+name, emit indented `<select>` (AC: #3)
  - [ ] Depth = number of ancestors (check if stored on term or computed from parent chain)
  - [ ] Two spaces of `&nbsp;` per depth level (configurable constant)
- [ ] Ensure submitted term ID routes to `HasTagOrDescendants` filter correctly (AC: #4)
  - [ ] Term ID is a `i64`; `FilterValue::Int` should already work with `HasTagOrDescendants` — verify in `query_builder.rs`
- [ ] Handle empty vocabulary: return select with only "Any" option (AC: #6)
- [ ] Update ritrovo_importer `field_topics` filter JSON (AC: #7)
- [ ] Write tests (AC: #8)

## Dev Notes

- `render_exposed_filter_form` currently has signature `fn render_exposed_filter_form(query: &GatherQuery, filter_values: &HashMap<String, String>, base_path: &str) -> String`. Adding `state: &AppState` requires updating all call sites (there should be one or two in the file).
- The render function is sync. Loading terms from `CategoryService` is likely async. Options:
  - (A) Pre-fetch terms before calling render, pass as a `HashMap<String, Vec<Term>>` into the function — keeps render fn sync and testable without a DB
  - (B) Make render async — requires wider changes
  - Option A is preferred: gather the async data upfront in the route handler, pass it in.
- Check `crates/kernel/src/services/category_service.rs` for the method to load terms by vocabulary name. The return type likely includes `id`, `name`, `parent_id`.
- Depth calculation: if terms have a `depth` column use it directly; otherwise compute from the parent chain. Check the term schema.
- `is_valid_field_name` is not needed here since vocabulary name comes from the query definition (trusted config), not user input.

### Project Structure Notes

- `crates/kernel/src/gather/types.rs` — `ExposedWidget::TaxonomySelect`
- `crates/kernel/src/routes/gather.rs` — `render_exposed_filter_form` signature update, `render_taxonomy_select` helper
- `crates/kernel/src/services/category_service.rs` — term loading API (read-only)
- `plugins/ritrovo_importer/src/lib.rs` — `field_topics` filter JSON

### References

- [Source: crates/kernel/src/gather/types.rs] — `ExposedWidget` enum (after Story 34.1)
- [Source: crates/kernel/src/routes/gather.rs#477] — `render_exposed_filter_form` to extend
- [Source: crates/kernel/src/services/category_service.rs] — term loading methods
- [Source: crates/plugin-sdk/src/types.rs] — `FieldType::Category` reference

## Dev Agent Record

### Agent Model Used

claude-sonnet-4-6

### Debug Log References

### Completion Notes List

### File List
