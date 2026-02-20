# Story 27.9: Update CLAUDE.md with Security Rules

Status: done

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

- [x] Add "Security Rules" section to CLAUDE.md (AC: #1, #2, #3)
  - [x] Document format whitelisting rule: always use `FilterPipeline::for_format_safe()`, never `for_format()` for user/plugin content
  - [x] Document HTML escaping rule: all user content interpolated into HTML must use `html_escape()` or Tera autoescape
  - [x] Document CSRF rule: all state-changing endpoints must use `require_csrf`
  - [x] Document SQL safety rule: never use `format!` for SQL, always use SeaQuery parameterized queries
  - [x] Document `| safe` rule: every `| safe` usage requires a comment justifying pre-sanitization
  - [x] Document WASM boundary rule: all plugin-supplied data must be validated/escaped before HTML interpolation
  - [x] Document file upload rule: validate content type, sanitize filenames, block executables
- [x] Document prohibited patterns (AC: #2)
  - [x] Raw SQL string interpolation (`format!` with SQL)
  - [x] `FilterPipeline::for_format()` with user/plugin-supplied format strings
  - [x] `| safe` without justification comment
  - [x] Unescaped user content in inline HTML construction
  - [x] Trusting plugin-supplied tag names, class names, or attribute keys without validation
- [x] Document required patterns (AC: #3)
  - [x] `FilterPipeline::for_format_safe()` for all view-time format processing
  - [x] `require_csrf` on all POST/PUT/DELETE form handlers
  - [x] `html_escape()` for user content in string-built HTML
  - [x] `is_valid_attr_key()` for plugin-supplied attribute keys
  - [x] `SAFE_TAGS` validation for plugin-supplied tag names
- [x] Add cross-reference to security audit doc (AC: #4)

## Implementation Details

### Section Structure

Added "Security Rules" section to CLAUDE.md between "Error Handling Rules" and "Kernel Minimality Rules" with 8 subsections:

1. **Format Processing** — `for_format_safe()` rule, `| safe` justification requirement
2. **HTML & XSS Prevention** — `html_escape()`, `SAFE_TAGS`, `is_valid_attr_key()`
3. **SQL Injection Prevention** — parameterized queries, `is_valid_field_name()`, `escape_like_pattern()`
4. **CSRF Protection** — `require_csrf` on state-changing endpoints, POST-only logout
5. **Authentication & Sessions** — Argon2id params, `session.cycle_id()`, password length
6. **WASM Plugin Boundary** — data validation, `statement_timeout`, key namespacing
7. **File Upload Security** — magic bytes, `sanitize_filename()`, MIME allowlist
8. **Prohibited Patterns** — consolidated list of banned patterns

### Cross-Reference

Opening line references `docs/security-audit.md` for dependency audit policy and Epic 27 story files for detailed findings.

### Files Changed

- `CLAUDE.md` — Added "Security Rules" section (lines 41-93)

## Dev Notes

### Dependencies

This story depends on the completion of stories 27-1 through 27-6 so that all findings and patterns are known. However, it can be started with 27-1 findings and updated as later audits complete.

### Existing CLAUDE.md Structure

The current CLAUDE.md already has sections for:
- Commit Messages
- Code Deduplication Rules
- Coding Standards
- Error Handling Rules
- **Security Rules** (NEW — added by this story)
- Kernel Minimality Rules
- Before Committing Checklist

### References

- [Source: CLAUDE.md — current structure and style]
- [Source: _bmad-output/implementation-artifacts/stories/27-1-xss-audit-and-hardening.md — XSS findings and patterns]
