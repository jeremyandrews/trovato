# Story 40.1: Skip Link and Page Landmarks

Status: ready-for-dev

## Story

As a **keyboard or screen reader user**,
I want a skip link as the first focusable element on every page,
so that I can bypass navigation and jump directly to content.

## Acceptance Criteria

1. `base.html` includes `<a href="#main-content" class="skip-link">Skip to main content</a>` as the first child of `<body>` (inside `{% block body %}`)
2. `<main>` element has `id="main-content"` to serve as the skip link target
3. Skip link is visually hidden by default, visible on focus (CSS: `.skip-link` positioned off-screen, `.skip-link:focus` positioned on-screen)
4. `<article>` wrapper added around item content in `templates/elements/item.html` (and any type-specific item templates)
5. Gather listing items wrapped in `<article>` in default gather row templates
6. All existing `aria-label`, `aria-current` usages verified still correct after template changes
7. No visual regression in default theme (skip link hidden until focused)

## Tasks / Subtasks

- [ ] Add skip link to `templates/base.html` as first child of `<body>` (AC: #1)
  - [ ] Add `<a href="#main-content" class="skip-link">Skip to main content</a>` before `<header>`
- [ ] Add `id="main-content"` to `<main>` element in `templates/base.html` (AC: #2)
- [ ] Add `.skip-link` CSS to `base.html` `<style>` block (AC: #3)
  - [ ] `.skip-link` positioned off-screen by default (`position: absolute; left: -9999px;`)
  - [ ] `.skip-link:focus` positioned on-screen with visible styling
- [ ] Wrap item content in `<article>` in `templates/elements/item.html` (AC: #4)
  - [ ] Check `templates/elements/item--*.html` type-specific templates for consistency
- [ ] Wrap gather listing items in `<article>` in default gather row templates (AC: #5)
- [ ] Audit all existing `aria-label`, `aria-current` usages across templates (AC: #6)
  - [ ] Verify pager `aria-label="Pagination"` still correct
  - [ ] Verify admin tabs `aria-current="page"` still correct
- [ ] Verify no visual regression with skip link hidden (AC: #7)

## Dev Notes

### Architecture
- `templates/base.html` -- add skip link before `<header>`, add `id="main-content"` to `<main>`
- `templates/elements/item.html` -- wrap content in `<article>`
- `templates/page.html` -- check for consistency with landmark structure
- `templates/elements/item--*.html` -- type-specific templates need matching `<article>` wrapper

### Security
- No security considerations -- template-only changes with no user input handling

### Testing
- Visual inspection: skip link hidden by default, visible on Tab key focus
- Screen reader verification: skip link announced, landmarks identified
- Template rendering: verify `<article>` wrapper does not break existing CSS selectors

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] -- Epic 40 definition
- [Source: templates/base.html] -- Base template with existing landmark structure
- [Source: templates/elements/item.html] -- Item display template
