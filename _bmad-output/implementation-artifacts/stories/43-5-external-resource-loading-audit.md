# Story 43.5: External Resource Loading Audit

Status: ready-for-dev

## Story

As a **privacy-conscious site operator**,
I want the default theme to load no external resources,
so that no third-party tracking occurs without explicit opt-in.

## Acceptance Criteria

1. Audit all templates (93+ files) for external URLs: no `<link>`, `<script>`, `<img>`, `<iframe>`, `@import`, `url()` referencing external domains
2. Audit `static/` directory for references to external resources (CDNs, analytics, fonts)
3. Audit `base.html` CSS for `@font-face` rules loading from external URLs
4. Verify render pipeline does not inject external resources (no hardcoded CDN URLs in Rust code)
5. Document the "no external resources" policy in operational docs
6. If any external resources are found, replace with local equivalents or remove
7. Add a CI check (grep-based) that flags new external URLs in template files

## Tasks / Subtasks

- [ ] Audit all template files for `https?://` references to external domains (AC: #1)
- [ ] Audit `static/` directory files for external resource references (AC: #2)
- [ ] Audit `base.html` for `@font-face` or `@import` with external URLs (AC: #3)
- [ ] Search Rust source for hardcoded CDN or external URLs in render pipeline (AC: #4)
- [ ] Replace any discovered external resources with local equivalents or remove them (AC: #6)
- [ ] Document "no external resources" policy in operational docs (AC: #5)
- [ ] Add CI check script that greps templates for external URLs and fails on matches (AC: #7)
- [ ] Add CI check to CI pipeline configuration (AC: #7)

## Dev Notes

### Architecture

- This is primarily a verification story, not a development story. The current state is likely clean (no CDN fonts, no analytics) but must be confirmed and documented.
- Audit scope: `templates/` directory (93+ files), `static/` directory, Rust source files in `crates/kernel/src/theme/` and `crates/kernel/src/routes/`.
- CI check: a shell script using `grep -rn 'https\?://' templates/ static/` with exclusions for localhost/internal URLs, integrated into the CI pipeline.

### Security

- External resource loading is a privacy concern: third-party servers receive user IP addresses and can set tracking cookies.
- Google Fonts CDN, analytics scripts, and externally hosted CSS/JS are the most common offenders.
- The "no external resources" policy means the default theme is GDPR-friendly out of the box -- no cookie consent needed for the base installation.

### Testing

- The CI check itself serves as the ongoing test -- any future PR that introduces an external URL in templates will fail CI.
- Manual verification during this story: run the grep audit, review results, confirm zero external URLs.

### References

- [Source: docs/ritrovo/epic-13-privacy.md -- Story 43.5]
- [Source: templates/ -- Template directory]
- [Source: static/ -- Static assets directory]
- [Source: crates/kernel/src/theme/ -- Render pipeline]
