# Story 27.7: Security Regression Tests

Status: ready-for-dev

## Story

As a **maintainer**,
I want every security finding converted to a regression test,
So that fixed vulnerabilities can never be reintroduced.

## Acceptance Criteria

1. Every Critical/High finding from stories 27.1-27.6 has a corresponding test
2. Tests verify the vulnerability is blocked (not just that the happy path works)
3. Tests are clearly labeled as security regression tests
4. Tests must never be removed without security review

## Tasks / Subtasks

- [ ] Inventory all Critical/High findings from stories 27.1-27.6 (AC: #1)
  - [ ] Collect findings from 27-1 (XSS) story file
  - [ ] Collect findings from 27-2 through 27-6 story files (as they complete)
- [ ] Write regression tests for XSS findings (AC: #1, #2)
  - [ ] Test format whitelisting rejects `full_html` at all render sites
  - [ ] Test SAFE_TAGS rejects dangerous tag names (script, iframe, input, link, meta)
  - [ ] Test class value escaping prevents attribute injection
  - [ ] Test attribute key validation rejects injection attempts
  - [ ] Test snippet sanitization blocks script tags while preserving mark tags
- [ ] Write regression tests for SQL injection findings (AC: #1, #2)
- [ ] Write regression tests for CSRF findings (AC: #1, #2)
- [ ] Write regression tests for auth/session findings (AC: #1, #2)
- [ ] Write regression tests for WASM sandbox findings (AC: #1, #2)
- [ ] Write regression tests for file upload findings (AC: #1, #2)
- [ ] Add `// SECURITY REGRESSION TEST` comment markers to all security tests (AC: #3)
- [ ] Add module-level doc comment noting tests must not be removed without security review (AC: #4)

## Dev Notes

### Dependencies

This story depends on the completion of stories 27-1 through 27-6. It should be developed after those audits are complete so the full findings inventory is available.

### Approach

Each security finding should get a test that:
1. Constructs the specific attack payload that the finding described
2. Passes it through the code path that was vulnerable
3. Asserts the output is safe (escaped, rejected, sanitized)

Tests should NOT just test the happy path — they must test the adversarial case.

### Existing Tests from 27-1

Story 27-1 already added 15 security-focused unit tests:
- `filter.rs`: `for_format_safe_rejects_full_html`, `for_format_safe_rejects_unknown`, `for_format_safe_allows_plain_text`, `for_format_safe_allows_filtered_html`
- `render.rs`: `test_render_markup_rejects_unsafe_tag`, `test_render_markup_allows_safe_tags`, `test_classes_to_string_escapes_quotes`, `test_process_value_rejects_full_html`, `test_valid_attr_keys`, `test_invalid_attr_keys`, `test_get_extra_attrs_skips_invalid_keys`
- `search.rs`: `sanitize_snippet_preserves_mark_tags`, `sanitize_snippet_escapes_script_tags`, `sanitize_snippet_escapes_all_non_mark_html`, `sanitize_snippet_handles_plain_text`

These should be audited and enhanced (more attack vectors) rather than duplicated. Additional tests from 27-2 through 27-6 findings will be added here.

### Test Organization

Security regression tests should be grouped in dedicated `#[cfg(test)]` modules or clearly marked within existing test modules. The `// SECURITY REGRESSION TEST` marker makes them greppable for audit purposes.

### References

- [Source: _bmad-output/implementation-artifacts/stories/27-1-xss-audit-and-hardening.md — XSS findings]
- [Source: _bmad-output/planning-artifacts/epics.md — Epic 27 story definitions]
