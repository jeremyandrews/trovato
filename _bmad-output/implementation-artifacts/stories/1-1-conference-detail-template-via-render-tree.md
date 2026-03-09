# Story 1.1: Conference Detail Template via Render Tree

Status: ready-for-dev

## Story

As a site visitor,
I want to view a conference detail page with title, dates, location, description, and external links,
So that I can learn about a conference before deciding to attend.

## Acceptance Criteria

1. Conference detail page renders through the Render Tree pipeline (Build → Alter → Sanitize → Render)
2. Type-specific template `item--conference.html` resolved via specificity chain (`item--conference--{id}.html` → `item--conference.html` → `item.html`)
3. Page displays: title, dates, location, description, and external links
4. All user-supplied content is HTML-escaped by the Render Tree (NFR1)

## Tasks / Subtasks

- [ ] Create `templates/elements/item--conference.html` extending base item template (AC: #1, #2)
  - [ ] Template uses Tera variables: `title`, `fields.field_start_date`, `fields.field_city`, etc.
  - [ ] Include CSS styling in `<style>` block
- [ ] Verify Render Tree pipeline processes conference items through Build → Alter → Sanitize → Render (AC: #1)
- [ ] Verify template specificity chain resolves correctly (AC: #2)
- [ ] Verify HTML escaping of user-supplied content (AC: #4)

## Dev Notes

### Architecture

- Render Tree already implemented in kernel: `crates/kernel/src/theme/render.rs`, `theme/engine.rs`, `crates/plugin-sdk/src/render.rs`
- Template engine: `ThemeEngine` in `crates/kernel/src/theme/engine.rs` handles suggestion resolution
- Template directory: `templates/elements/` for item templates
- Specificity chain: `item--{type}--{id}.html` → `item--{type}.html` → `item.html`
- Conference item type already defined in Part 1 with fields: title, field_start_date, field_end_date, field_city, field_country, field_description, field_website, field_cfp_url, field_cfp_deadline

### Security

- All user content goes through Render Tree sanitization — no raw HTML from plugins
- Tera autoescape is on by default — `| safe` requires `{# SAFE: reason #}` comment
- Use `FilterPipeline::for_format_safe()` for any text format processing

### Testing

- Integration tests use `#[test]` + `run_test(async { ... })` on `SHARED_RT` runtime
- Test via HTTP: `GET /item/{id}` should return HTML with conference-specific template
- Verify template contains expected CSS classes and field values

### References

- [Source: docs/design/Design-Render-Theme.md] — Render Tree pipeline design
- [Source: docs/tutorial/plan-parts-03-04.md#Step 1] — Tutorial step details
- [Source: crates/kernel/src/theme/engine.rs] — ThemeEngine implementation
- [Source: crates/kernel/src/theme/render.rs] — RenderElement types
