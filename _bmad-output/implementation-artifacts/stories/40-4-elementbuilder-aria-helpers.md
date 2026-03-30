# Story 40.4: ElementBuilder ARIA Helpers

Status: ready-for-dev

## Story

As a **plugin developer** building accessible UI components,
I want ARIA-specific helper methods on `ElementBuilder`,
so that I can add accessibility attributes with compile-time safety and discoverability.

## Acceptance Criteria

1. `ElementBuilder` gains methods: `.aria_label(s)`, `.aria_describedby(id)`, `.aria_hidden(bool)`, `.aria_current(s)`, `.aria_live(s)`, `.role(s)`, `.aria_expanded(bool)`, `.aria_controls(id)`
2. Each method maps to the corresponding HTML attribute (e.g., `.aria_label("Search")` produces `aria-label="Search"`)
3. Methods are additive (can call multiple on the same builder)
4. Existing `.attr("aria-label", "...")` usage continues to work (helpers are sugar, not replacement)
5. Documentation with examples in doc comments for each method
6. At least one existing plugin or kernel usage updated to demonstrate (e.g., pager render element)

## Tasks / Subtasks

- [ ] Add `.aria_label(s)` method to `ElementBuilder` (AC: #1, #2)
  - [ ] Internally calls `.attr("aria-label", s)`
  - [ ] Add doc comment with usage example
- [ ] Add `.aria_describedby(id)` method (AC: #1, #2)
  - [ ] Internally calls `.attr("aria-describedby", id)`
  - [ ] Add doc comment with usage example
- [ ] Add `.aria_hidden(bool)` method (AC: #1, #2)
  - [ ] Internally calls `.attr("aria-hidden", "true"/"false")`
  - [ ] Add doc comment with usage example
- [ ] Add `.aria_current(s)` method (AC: #1, #2)
  - [ ] Internally calls `.attr("aria-current", s)`
  - [ ] Add doc comment with usage example
- [ ] Add `.aria_live(s)` method (AC: #1, #2)
  - [ ] Internally calls `.attr("aria-live", s)`
  - [ ] Add doc comment with usage example
- [ ] Add `.role(s)` method (AC: #1, #2)
  - [ ] Internally calls `.attr("role", s)`
  - [ ] Add doc comment with usage example
- [ ] Add `.aria_expanded(bool)` method (AC: #1, #2)
  - [ ] Internally calls `.attr("aria-expanded", "true"/"false")`
  - [ ] Add doc comment with usage example
- [ ] Add `.aria_controls(id)` method (AC: #1, #2)
  - [ ] Internally calls `.attr("aria-controls", id)`
  - [ ] Add doc comment with usage example
- [ ] Verify methods are additive and composable (AC: #3)
- [ ] Verify existing `.attr("aria-label", ...)` usage still works (AC: #4)
- [ ] Update at least one existing kernel usage to use new helpers (AC: #6)
  - [ ] Identify candidate (e.g., pager `aria-label`, admin tabs `aria-current`)
  - [ ] Replace `.attr()` call with new helper method

## Dev Notes

### Architecture
- `crates/plugin-sdk/src/types.rs` -- `ElementBuilder` impl block is the only file to modify
- These are pure convenience methods that call `.attr()` internally
- No breaking change to existing plugins -- additive API only
- Boolean methods (`.aria_hidden()`, `.aria_expanded()`) should convert `bool` to `"true"`/`"false"` string

### Security
- No security impact -- helpers produce the same output as manual `.attr()` calls
- Attribute values are escaped by the render pipeline in `theme/render.rs` (via `html_escape()`)

### Testing
- Unit test: build element with each ARIA helper, verify attribute map contains correct key/value
- Unit test: chain multiple ARIA helpers on one builder, verify all present
- Unit test: mix `.attr()` and ARIA helpers on same builder

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] -- Epic 40 definition
- [Source: crates/plugin-sdk/src/types.rs] -- ElementBuilder implementation
- [Source: docs/design/Design-Plugin-SDK.md] -- Plugin SDK API reference
