# Story 34.1: Boolean Exposed Filter Widget

Status: ready-for-dev

## Story

As a **Ritrovo visitor**,
I want the "Online Only" filter to show a Yes / No / Any dropdown instead of a text box,
so that I can filter without guessing whether to type `true`, `1`, or `yes`.

## Acceptance Criteria

1. `QueryFilter` gains an optional `widget` field (`"widget": "boolean"` in JSON); default is text input (no change to existing filters)
2. When `widget == "boolean"`, the exposed filter renders as a `<select>` with three options: `Any` (value `""`), `Yes` (value `"true"`), `No` (value `"false"`)
3. Submitted values `"true"` / `"false"` are parsed to `FilterValue::Bool` when the operator is `Equals` and the widget is boolean
4. `collect_exposed_filters` includes the widget type in the returned metadata JSON for templates
5. `render_exposed_filter_form` correctly pre-selects the current value on re-render (after form submit)
6. Ritrovo `field_online` filter in the "Upcoming Conferences" gather updated to `"widget": "boolean"`
7. All existing exposed filter tests continue to pass (text widget remains default)
8. New unit tests cover: boolean select renders with correct options, current value pre-selected, `"true"` → `FilterValue::Bool(true)` parsing

## Tasks / Subtasks

- [ ] Add `ExposedWidget` enum to `crates/kernel/src/gather/types.rs` (AC: #1)
  - [ ] `Text` variant (default) and `Boolean` variant
  - [ ] `#[serde(rename_all = "snake_case")]` so `"boolean"` deserializes to `ExposedWidget::Boolean`
  - [ ] Add `#[serde(default)]` on the new `widget: ExposedWidget` field of `QueryFilter`
  - [ ] Derive `PartialEq` + `Default` on `ExposedWidget`; `Default` returns `Text`
- [ ] Update `render_exposed_filter_form` in `crates/kernel/src/routes/gather.rs` (AC: #2, #5)
  - [ ] Branch on `ExposedWidget::Boolean`: render `<select>` with Any/Yes/No options
  - [ ] Pre-select the option matching current filter value
- [ ] Update filter value parsing so `"true"`/`"false"` strings become `FilterValue::Bool` for boolean widgets (AC: #3)
  - [ ] Identify where URL param strings are resolved to `FilterValue` in `gather_service.rs` or `routes/gather.rs`
  - [ ] Apply boolean coercion only when `widget == Boolean` to avoid breaking other string-valued filters
- [ ] Update `collect_exposed_filters` to include `"widget"` key in returned JSON (AC: #4)
- [ ] Update ritrovo_importer: add `"widget": "boolean"` to the `fields.field_online` filter in the "Upcoming Conferences" gather query definition (AC: #6)
- [ ] Write unit tests (AC: #7, #8)

## Dev Notes

- `ExposedWidget` enum lives in `gather/types.rs` alongside `QueryFilter`.
- `render_exposed_filter_form` is in `crates/kernel/src/routes/gather.rs` lines 477–524. Currently renders every filter as `<input type="text">`.
- Use `// Infallible:` comment on any `write!(string, ...).unwrap()` that writes to a `String`.
- The value parsing path: URL query params arrive as `&str` → `HashMap<String, String>` (in `parse_filter_params`) → passed to `GatherService::execute` → filter values matched against `QueryFilter.value` defaults. For boolean widget, the submitted `"true"`/`"false"` string needs to become `FilterValue::Bool` before the query builder sees it.
- `"widget"` field must be skipped during serialization of the query definition if it is the default (text), to keep existing query JSON compact. Use `#[serde(skip_serializing_if = "ExposedWidget::is_text")]` or similar.

### Project Structure Notes

- `crates/kernel/src/gather/types.rs` — `QueryFilter` struct, new `ExposedWidget` enum
- `crates/kernel/src/routes/gather.rs` — `render_exposed_filter_form`, `collect_exposed_filters`, `parse_filter_params`
- `plugins/ritrovo_importer/src/lib.rs` — `field_online` filter JSON in `seed_gather_queries`

### References

- [Source: crates/kernel/src/gather/types.rs#83] — `QueryFilter` struct
- [Source: crates/kernel/src/routes/gather.rs#477] — `render_exposed_filter_form` current implementation
- [Source: plugins/ritrovo_importer/src/lib.rs] — `field_online` filter (search `"field_online"`)

## Dev Agent Record

### Agent Model Used

claude-sonnet-4-6

### Debug Log References

### Completion Notes List

### File List
