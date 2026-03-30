# Story 48.1: Inclusivity-First Narrative Integration

Status: ready-for-dev

## Story

As a tutorial reader learning Trovato,
I want foundational concepts (accessibility, i18n, security, privacy, performance) woven naturally into the narrative,
so that I finish the tutorial already knowing Trovato is accessible, internationalized, and secure without ever reading a dedicated chapter.

## Acceptance Criteria

1. Part 3, when introducing templates, shows semantic landmarks (`<nav>`, `<main>`, `<footer>`) and a skip link (`<a href="#main-content">`) with a brief explanation of why they matter.
2. Part 3 render pipeline note explains semantic fallback behavior and demonstrates `loading="lazy"` on images for performance.
3. Part 5 form validation demonstrates `aria-describedby` linking error messages to their fields.
4. Part 5 image blocks note that `alt` is required, with a Drupal bridge explanation ("In Drupal you'd configure alt as required on the image field; Trovato enforces this at the field definition level").
5. Part 7 demonstrates `dir="rtl"` on the `<html>` element and locale-aware date formatting.
6. No separate "accessibility chapter" or "security chapter" exists; all concepts are integrated into the relevant narrative flow.
7. Tutorial bridge language is used when introducing concepts (e.g., "Taps serve the same role as Drupal hooks", "Tiles are Trovato's equivalent of Drupal blocks").

## Tasks / Subtasks

- [ ] Add semantic landmarks and skip link example to Part 3 template introduction section (AC: #1)
- [ ] Add render pipeline note about semantic fallback and `loading="lazy"` to Part 3 (AC: #2)
- [ ] Add `aria-describedby` form validation example to Part 5 (AC: #3)
- [ ] Add required `alt` attribute note with Drupal bridge to Part 5 image blocks section (AC: #4)
- [ ] Add `dir="rtl"` and locale-aware date formatting examples to Part 7 (AC: #5)
- [ ] Audit all 7 parts to confirm no standalone accessibility/security/i18n chapters exist (AC: #6)
- [ ] Audit all bridge language for accuracy and consistency with CLAUDE.md terminology (AC: #7)
- [ ] Review all changes for narrative flow — concepts should feel natural, not bolted on (AC: #1, #2, #3, #4, #5)

## Dev Notes

### Architecture

The goal is narrative integration, not additive sections. Each concept should appear at the moment it is most natural in the tutorial flow:

- Semantic HTML when the reader first writes a template (Part 3)
- Form accessibility when the reader first builds a form (Part 5)
- i18n when the reader first encounters localization (Part 7)

Bridge language helps readers coming from Drupal map concepts without a glossary detour.

### Testing

- Read each modified section aloud to verify narrative flow.
- Verify all code examples compile/render correctly against the current Trovato kernel.
- Cross-reference CLAUDE.md terminology rules (category not taxonomy, item not node, tap not hook, tile not block, plugin not module, gather not views).

### References

- `docs/tutorial/part-03-look-and-feel.md`
- `docs/tutorial/part-05-forms-and-input.md`
- `docs/tutorial/part-07-going-global.md`
- `CLAUDE.md` — Trovato terminology section
