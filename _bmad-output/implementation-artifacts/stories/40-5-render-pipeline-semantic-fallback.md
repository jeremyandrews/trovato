# Story 40.5: Render Pipeline Semantic Fallback

Status: ready-for-dev

## Story

As a **theme developer**,
I want the Rust-side render fallback to produce semantic HTML,
so that pages without custom templates are still accessible.

## Acceptance Criteria

1. When no template matches a render element, the Rust fallback uses semantic tags: `<article>` for item-type elements, `<section>` for container elements with headings, `<nav>` for navigation elements, `<div>` only for generic containers
2. Heading elements use the correct `<h1>`-`<h6>` tag based on `heading_level` in block data (not generic `<div>`)
3. Image elements in fallback output include `alt` attribute (Note: `loading="lazy"` is handled by Story 44.4, not this story)
4. Link elements use `<a>` with proper `href` (not `<div>` with click handler)
5. Fallback output tested with at least 3 render element types (item, heading block, image block)

## Tasks / Subtasks

- [ ] Map RenderElement `#type` values to semantic HTML tags in fallback path (AC: #1)
  - [ ] Item-type elements use `<article>`
  - [ ] Container elements with headings use `<section>`
  - [ ] Navigation elements use `<nav>`
  - [ ] Generic containers fall back to `<div>`
- [ ] Render heading elements with correct `<h1>`-`<h6>` tag (AC: #2)
  - [ ] Read `heading_level` from block data
  - [ ] Clamp to valid range (1-6), default to `<h2>` if unspecified
- [ ] Render image elements with `alt` attribute (AC: #3)
  - [ ] Include `alt` attribute from block data (default to empty string)
  - [ ] Note: `loading="lazy"` injection is Story 44.4's responsibility — do not duplicate here
- [ ] Render link elements with `<a>` tag and `href` (AC: #4)
  - [ ] Extract `href` from block/element data
  - [ ] Use `<a>` tag instead of `<div>`
- [ ] Add tests for semantic fallback (AC: #5)
  - [ ] Test item-type element renders as `<article>`
  - [ ] Test heading block renders with correct `<h1>`-`<h6>` tag
  - [ ] Test image block renders with `alt` attribute

## Dev Notes

### Architecture
- `crates/kernel/src/theme/render.rs` -- `render_element()` fallback path is the primary modification target
- The fallback path is used when no Tera template matches the suggestion chain
- Map `RenderElement` `#type` field to semantic tags:
  - `"item"` / `"item--*"` -> `<article>`
  - `"container"` with heading child -> `<section>`
  - `"navigation"` / `"nav"` -> `<nav>`
  - Everything else -> `<div>`
- Plugin-supplied tag names must still be validated against `SAFE_TAGS` allowlist

### Security
- All attribute values must go through `html_escape()` in the render pipeline
- `alt` text from block data must be escaped (handled by existing attribute rendering)
- `href` values from block data must be escaped (handled by existing attribute rendering)
- Tag names in headings are generated from code (`h1`-`h6`), not user input -- safe

### Testing
- Unit tests in `render.rs` for each semantic mapping
- Test fallback with item RenderElement -> verify `<article>` tag in output
- Test fallback with heading block -> verify correct `<h1>`-`<h6>` tag
- Test fallback with image block -> verify `alt` attribute present

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] -- Epic 40 definition
- [Source: crates/kernel/src/theme/render.rs] -- Render pipeline fallback path
- [Source: docs/design/Design-Render-Theme.md] -- Render Tree pipeline design
