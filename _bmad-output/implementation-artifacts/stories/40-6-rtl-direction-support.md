# Story 40.6: RTL Direction Support

Status: ready-for-dev

## Story

As a **site serving content in right-to-left languages** (Arabic, Hebrew, Farsi),
I want the `dir` attribute set correctly on `<html>`,
so that text direction renders correctly throughout the page.

## Acceptance Criteria

1. `base.html` sets `dir="{{ text_direction | default(value='ltr') }}"` on the `<html>` element
2. Kernel middleware populates `text_direction` in the template context based on active language
3. A lookup table of RTL language codes exists in the language middleware (ar, he, fa, ur, ps, yi, etc.)
4. Default CSS in `base.html` uses logical properties where physical direction was previously used: `margin-inline-start/end` instead of `margin-left/right`, `padding-inline-start/end` instead of `padding-left/right`, `text-align: start` instead of `text-align: left`
5. Admin UI CSS also uses logical properties (admin templates extend `base.html`)
6. No visual regression for LTR sites (logical properties resolve identically to physical for LTR)

## Tasks / Subtasks

- [ ] Add `dir` attribute to `<html>` element in `templates/base.html` (AC: #1)
  - [ ] Set `dir="{{ text_direction | default(value='ltr') }}"`
- [ ] Add `text_direction` to template context from middleware (AC: #2)
  - [ ] Modify language middleware in `crates/kernel/src/middleware/language.rs`
  - [ ] Populate `text_direction` based on active language code
- [ ] Create `RTL_LANGUAGES` lookup table (AC: #3)
  - [ ] Add constant with ISO 639-1 codes: ar, he, fa, ur, ps, yi, arc, ckb, dv, ha, khw, ks, ku, sd, ug
  - [ ] Place in `crates/kernel/src/middleware/language.rs`
- [ ] Convert CSS to logical properties in `templates/base.html` (AC: #4)
  - [ ] Replace `margin-left`/`margin-right` with `margin-inline-start`/`margin-inline-end`
  - [ ] Replace `padding-left`/`padding-right` with `padding-inline-start`/`padding-inline-end`
  - [ ] Replace `text-align: left` with `text-align: start`
  - [ ] Replace `float: left`/`float: right` with `float: inline-start`/`float: inline-end` where supported
- [ ] Verify admin UI CSS uses logical properties (AC: #5)
  - [ ] Check admin templates that extend `base.html` for physical direction properties
  - [ ] Convert any remaining physical properties to logical equivalents
- [ ] Verify no visual regression for LTR (AC: #6)
  - [ ] Logical properties resolve identically to physical for `dir="ltr"`

## Dev Notes

### Architecture
- `templates/base.html` -- add `dir` attribute to `<html>`, convert CSS to logical properties
- `crates/kernel/src/middleware/language.rs` -- add `RTL_LANGUAGES` constant and `text_direction` context population
- Note: `loading="lazy"` on images is handled by Story 44.4 (Epic E), not this story
- This story establishes the RTL/direction foundation that Epic B (i18n) builds on

### Security
- The `dir` attribute value comes from a controlled lookup (either `"ltr"` or `"rtl"`), not user input -- safe
- Logical properties are CSS features with broad browser support -- no security implications

### Testing
- Unit test: verify `RTL_LANGUAGES` contains expected language codes (ar, he, fa, ur)
- Unit test: verify middleware sets `text_direction` to `"rtl"` for Arabic, `"ltr"` for English
- Visual test: compare LTR rendering before and after CSS logical property conversion
- Template test: verify `dir` attribute present on `<html>` element in rendered output

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] -- Epic 40 definition
- [Source: templates/base.html] -- Base template
- [Source: crates/kernel/src/middleware/language.rs] -- Language middleware
- [Source: docs/ritrovo/epic-11-i18n.md] -- i18n epic that builds on RTL foundation
