# Story 27.9: Update CLAUDE.md with Security Rules

Status: ready-for-dev

## Story

As a **maintainer**,
I want CLAUDE.md to enforce security practices during AI-assisted development,
So that new code follows security best practices automatically.

## Acceptance Criteria

1. Security review requirements documented for PRs touching: user input handling, authentication, SQL queries, template rendering, WASM boundary, file uploads
2. Prohibited patterns documented (raw SQL interpolation, `| safe` without justification, `.unwrap()` on user input)
3. Required patterns documented (parameterized queries, `require_csrf` on state-changing endpoints, `html_escape` for user content)
4. Reference to `docs/security-audit.md` for full findings and rationale

## Tasks / Subtasks

- [ ] Add "Security Rules" section to CLAUDE.md (AC: #1, #2, #3)
  - [ ] Document format whitelisting rule: always use `FilterPipeline::for_format_safe()`, never `for_format()` for user/plugin content
  - [ ] Document HTML escaping rule: all user content interpolated into HTML must use `html_escape()` or Tera autoescape
  - [ ] Document CSRF rule: all state-changing endpoints must use `require_csrf`
  - [ ] Document SQL safety rule: never use `format!` for SQL, always use SeaQuery parameterized queries
  - [ ] Document `| safe` rule: every `| safe` usage requires a comment justifying pre-sanitization
  - [ ] Document WASM boundary rule: all plugin-supplied data must be validated/escaped before HTML interpolation
  - [ ] Document file upload rule: validate content type, sanitize filenames, block executables
- [ ] Document prohibited patterns (AC: #2)
  - [ ] Raw SQL string interpolation (`format!` with SQL)
  - [ ] `FilterPipeline::for_format()` with user/plugin-supplied format strings
  - [ ] `| safe` without justification comment
  - [ ] Unescaped user content in inline HTML construction
  - [ ] Trusting plugin-supplied tag names, class names, or attribute keys without validation
- [ ] Document required patterns (AC: #3)
  - [ ] `FilterPipeline::for_format_safe()` for all view-time format processing
  - [ ] `require_csrf` on all POST/PUT/DELETE form handlers
  - [ ] `html_escape()` for user content in string-built HTML
  - [ ] `is_valid_attr_key()` for plugin-supplied attribute keys
  - [ ] `SAFE_TAGS` validation for plugin-supplied tag names
- [ ] Add cross-reference to security audit doc (AC: #4)

## Dev Notes

### Dependencies

This story depends on the completion of stories 27-1 through 27-6 so that all findings and patterns are known. However, it can be started with 27-1 findings and updated as later audits complete.

### Existing CLAUDE.md Structure

The current CLAUDE.md already has sections for:
- Commit Messages
- Code Deduplication Rules
- Coding Standards
- Error Handling Rules
- Kernel Minimality Rules
- Before Committing Checklist

The new "Security Rules" section should follow the same concise, imperative style. It should be placed after "Error Handling Rules" and before "Kernel Minimality Rules" since security is closely related to error handling.

### Key Patterns from 27-1

From the XSS audit, the critical patterns to document are:
- `FilterPipeline::for_format_safe()` — the single source of truth for safe format processing (added in 27-1)
- `html_escape()` — from `crate::routes::helpers` for manual HTML construction
- `SAFE_TAGS` — in `theme/render.rs` for plugin tag validation
- `is_valid_attr_key()` — in `theme/render.rs` for plugin attribute validation
- `classes_to_string()` — now escapes values, but callers should be aware
- `sanitize_snippet()` — pattern for allowing specific safe HTML tags through escaping

### References

- [Source: CLAUDE.md — current structure and style]
- [Source: _bmad-output/implementation-artifacts/stories/27-1-xss-audit-and-hardening.md — XSS findings and patterns]
