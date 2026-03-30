# Story 41.3: Language Context Propagation Audit

Status: ready-for-dev

## Story

As a **kernel maintainer**,
I want `active_language` and `text_direction` available in all template render contexts,
So that every page, error response, and admin screen knows the current language.

## Acceptance Criteria

1. `active_language` is set in template context for: page renders, gather renders, admin pages, error pages (400, 403, 404, 500), installer pages
2. `text_direction` (from Epic 40) is set in the same contexts as `active_language`
3. API JSON responses include `Content-Language` header matching the negotiated language
4. Language negotiation middleware runs on all route groups: public pages, admin routes, API routes, file routes
5. Negotiation chain order verified: URL prefix -> cookie -> Accept-Language header -> site default
6. If URL prefix negotiation activates a language, the rewritten URL is used for routing (already implemented -- verify no regression)
7. Error pages rendered by the kernel (not plugin) use the negotiated language for their UI strings (button text, "Page not found" message)

## Tasks / Subtasks

- [ ] Audit all route handlers in `crates/kernel/src/routes/` that render templates -- verify each sets `active_language` in context (AC: #1)
  - [ ] Page render routes (item.rs, front.rs)
  - [ ] Gather render routes (gather_routes.rs, display.rs)
  - [ ] Admin routes (admin.rs, admin_*.rs)
  - [ ] Error pages (helpers.rs: render_error, render_server_error, render_not_found)
  - [ ] Installer pages (installer.rs)
- [ ] Audit same routes for `text_direction` in context (AC: #2)
- [ ] Audit API routes -- add `Content-Language` header to JSON responses (AC: #3)
- [ ] Verify language middleware is applied to all router layers in `routes/mod.rs` (AC: #4)
- [ ] Trace the negotiation chain in `middleware/language.rs` and verify order: URL prefix -> cookie -> Accept-Language -> site default (AC: #5)
- [ ] Test URL prefix rewriting -- verify rewritten URI reaches correct route handler (AC: #6)
- [ ] Verify error page templates use `trans` filter or negotiated language strings (AC: #7)
- [ ] Fill any gaps found during audit (AC: all)
- [ ] Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`

## Dev Notes

### Architecture

This is primarily an audit and gap-filling story, not new development. The language middleware (`crates/kernel/src/middleware/language.rs`) and `active_language` template variable already exist. The goal is to verify complete coverage and fix any gaps.

Key files to audit:
- `crates/kernel/src/routes/` -- all route handlers
- `crates/kernel/src/middleware/language.rs` -- negotiation middleware
- `crates/kernel/src/routes/helpers.rs` -- render_error, render_server_error, render_not_found
- `crates/kernel/src/routes/mod.rs` -- router layer configuration

### Security

API `Content-Language` header is informational only -- no security implications beyond correct behavior.

### Testing

- Verify existing language middleware tests still pass
- Add tests for any gaps found (e.g., error pages missing active_language)
- Test that API responses include Content-Language header

### References

- `crates/kernel/src/middleware/language.rs` -- 703 lines of negotiation logic
- `crates/kernel/src/routes/helpers.rs` -- error rendering functions
- [Epic 41 source: docs/ritrovo/epic-11-i18n.md]
