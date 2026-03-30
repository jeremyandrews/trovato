# Story 40.7: Focus Indicators and Tab Order Audit

Status: ready-for-dev

## Story

As a **keyboard-only user** administering a Trovato site,
I want visible focus indicators and correct tab order on all interactive elements,
so that I can see where I am and reach every control via keyboard.

## Acceptance Criteria

1. CSS `:focus-visible` outline is visible on all focusable elements — not suppressed by `outline: none`
2. `:focus-visible` styles added to `base.html` CSS (applies to both public and admin via inheritance)
3. All interactive admin elements (buttons, links, form controls) are reachable via Tab key — no `<div onclick>` patterns
4. Admin list tables: rows are not interactive — action buttons/links within rows are focusable
5. Delete confirmation actions require explicit activation (click or Enter/Space) — not triggered by focus alone
6. `.visually-hidden` CSS utility class added (positioned off-screen, not `display:none`) for screen-reader-only content
7. At least 1 integration test: verify no `outline: none` on focusable elements in rendered admin pages

## Tasks / Subtasks

- [ ] Add `:focus-visible` styles to `base.html` CSS (AC: #1, #2)
  - [ ] Ensure outline is visible on all focusable elements
  - [ ] Remove any existing `outline: none` declarations
  - [ ] Apply via inheritance to admin UI
- [ ] Add `.visually-hidden` CSS class to `base.html` (AC: #6)
  - [ ] Position off-screen, not `display:none` (screen readers must read it)
- [ ] Audit all interactive admin elements for Tab reachability (AC: #3)
  - [ ] Buttons, links, form controls must be natively focusable elements
  - [ ] Replace any `<div onclick>` patterns with `<button>` or `<a>`
- [ ] Verify admin table navigation (AC: #4)
  - [ ] Table rows are not interactive — action buttons/links within rows are focusable
  - [ ] Verify no `tabindex` on `<tr>` elements
- [ ] Verify delete actions require explicit activation (AC: #5)
  - [ ] Delete buttons/links require click or Enter/Space — not triggered on focus
  - [ ] Confirm delete patterns use `<button>` or `<a>` with proper semantics
- [ ] Write test verifying no `outline: none` suppression (AC: #7)

## Dev Notes

### Architecture
- `templates/base.html` — CSS `:focus-visible` styles, `.visually-hidden` class
- This is the foundation story (tab order + focus visibility). Stories 40.8 and 40.9 handle AJAX announcements and advanced keyboard patterns respectively.

### Security
- No security impact — UI-only changes with no data handling modifications
- Delete actions already require POST with CSRF — this story verifies they also require explicit keyboard activation

### Testing
- Manual keyboard testing: Tab through admin pages, verify every interactive element reachable
- Verify `:focus-visible` outline visible on buttons, links, inputs
- Verify delete buttons not activated by Tab focus alone

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] — Epic 40 definition
- [Source: templates/base.html] — Base template
- [Source: templates/admin/macros/] — Admin UI macro templates
