# Story 27.1: XSS Audit and Hardening

Status: done

## Story

As a **security reviewer**,
I want every code path that renders user input to HTML audited for XSS,
So that no unsanitized user content can reach the browser.

## Acceptance Criteria

1. Tera template engine autoescape configuration verified (globally enabled)
2. All uses of `| safe` or `| raw` filters in templates audited — each justified or removed
3. Render Tree sanitization confirmed — no code path bypasses sanitizer for user content
4. JSONB field content sanitization verified (on output, on input, or both — documented)
5. Admin-entered content that renders to non-admin users verified as sanitized
6. All findings documented with severity ratings
7. All Critical/High findings fixed

## Tasks / Subtasks

- [x] Verify Tera autoescape is enabled globally (AC: #1)
- [x] Audit all templates for `| safe` / `| raw` filter usage (AC: #2)
  - [x] Categorize each `| safe` usage: pre-sanitized render tree output vs. user content
  - [x] Verify search snippet `| safe` is safe (Finding A)
  - [x] Verify form element prefix/suffix `| safe` uses are admin-defined only
- [x] Trace Render Element rendering — verify sanitization on all user-content paths (AC: #3)
  - [x] Fix RenderElement attribute key escaping gap (Finding B)
- [x] Audit JSONB field content handling — storage vs rendering sanitization (AC: #4)
  - [x] Fix front page `render_item_fields` format whitelisting gap (Finding C)
  - [x] Fix comment `body_format` view-time whitelisting gap (Finding D)
- [x] Audit admin form inputs that render to public-facing pages (AC: #5)
- [x] Document all findings with severity ratings (AC: #6, #7)

## Dev Notes

### Codebase XSS Defenses (Confirmed Strengths)

The codebase has strong XSS defenses overall:

1. **Tera autoescape ON globally** — `crates/kernel/src/theme/engine.rs` enables autoescape for all `.html` templates. No `{% autoescape false %}` blocks exist anywhere. No `| raw` filter usage found.

2. **`html_escape()` coverage** — `crates/kernel/src/routes/helpers.rs` escapes all 5 critical characters (`&`, `<`, `>`, `"`, `'`). Used consistently across route handlers for titles, user names, and inline HTML construction.

3. **`ammonia` HTML sanitizer** — The `FilteredHtmlFilter` in `crates/kernel/src/content/filter.rs` uses the `ammonia` crate to strip dangerous tags/attributes while allowing safe HTML subsets.

4. **`FilterPipeline` defaults to `plain_text`** — `FilterPipeline::for_format()` at `content/filter.rs:43` falls through unknown formats to `plain_text()`, which escapes all HTML. This is the correct safe default.

5. **Block editor sanitization** — Each block type has per-type sanitization in the block editor rendering pipeline. The `ammonia` sanitizer is applied to paragraph/list/header blocks.

6. **`for_format_checked()` exists** — `content/filter.rs:51` provides a permission-gated variant that downgrades `full_html` to `filtered_html` when the user lacks permission. Used in `item.rs:253-255` for view rendering.

### Finding A (MEDIUM): Search Snippets Rendered Without Escaping

**Location:** `routes/search.rs:243-246`, `templates/search.html:24`

**Issue:** Search result snippets come from PostgreSQL `ts_headline()` which wraps matches in `<mark>...</mark>` tags. The snippets are rendered with `| safe` in the template and inserted without escaping in the Rust handler. PostgreSQL does HTML-escape the non-highlighted parts of `ts_headline` output by default, so this is defense-in-depth rather than an active exploit path.

**Fix applied:** Added `sanitize_snippet()` function that escapes the entire snippet via `html_escape()` then restores only `<mark>`/`</mark>` tags. Applied to all search results before they reach the template context or fallback renderer. 4 unit tests added.

### Finding B (LOW): RenderElement Attribute Keys Not Escaped

**Location:** `theme/render.rs:232-244`

**Issue:** In `get_extra_attrs()`, attribute values were escaped via `html_escape()`, but attribute keys were interpolated directly. If a plugin set an attribute key containing `"` or `>`, it could break out of the HTML tag.

**Fix applied:** Added `is_valid_attr_key()` validation requiring keys to match `[a-zA-Z][a-zA-Z0-9-_]*`. Invalid keys are silently skipped. Also added `html_escape()` to the non-string value fallback path (`_ => v.to_string()`). 3 unit tests added.

### Finding C (MEDIUM): Front Page Format Whitelisting Gap

**Location:** `routes/front.rs:141-142` and `routes/front.rs:176-177`

**Issue:** The front page passed DB-stored format directly to `FilterPipeline::for_format()` without whitelisting. If format was `"full_html"`, content passed through with no sanitization. The `item.rs` view handler correctly whitelisted.

**Fix applied:** Added format whitelisting at both locations: `match format { "plain_text" | "filtered_html" => format, _ => "plain_text" }`.

### Finding D (LOW): Comment `body_format` Not Whitelisted at View Time

**Location:** `routes/comment.rs:156`, `routes/comment.rs:293`, `routes/comment.rs:356`, `routes/comment.rs:471`

**Issue:** Comment rendering trusted the DB-stored `body_format` without whitelisting. The input path correctly constrained formats, but the output path did not verify.

**Fix applied:** Added whitelist check at all 4 render sites: `match body_format { "plain_text" | "filtered_html" => format, _ => "plain_text" }`.

### `| safe` Usage Audit (All 30+ Instances)

The `| safe` filter is used extensively but falls into these categories:

1. **Pre-sanitized Render Tree output** (SAFE): `{{ content | safe }}`, `{{ children | safe }}`, `{{ sidebar_tiles | safe }}` — These contain HTML already built by the Render Tree pipeline which sanitizes during construction. This is the correct pattern.

2. **Form element prefix/suffix** (SAFE): `{{ element.prefix | safe }}`, `{{ element.suffix | safe }}` — These are defined in code by form builders, not user input.

3. **Form element markup** (SAFE): `{{ element.element_type.value | safe }}` — Admin-defined markup elements.

4. **Search snippets** (FIXED): `{{ result.snippet | safe }}` — Now pre-sanitized by `sanitize_snippet()`.

5. **Comment body** (SAFE): `{{ comment.body_html | safe }}` — Pre-sanitized through `FilterPipeline` before template rendering.

6. **Link/markup element values** (SAFE): `{{ value | safe }}` in `elements/link.html` and `elements/markup.html` — Render Tree output, sanitized during construction.

### Admin Form Inputs Audit (AC: #5)

Admin-entered content that renders to non-admin users:
- **Site name/slogan** — Rendered via Tera autoescape in base.html. Safe.
- **Content fields** — Processed through `FilterPipeline` with format whitelisting (now including front page). Safe.
- **Category names** — Rendered via Tera autoescape. Safe.
- **Menu link titles** — Rendered via Tera autoescape. Safe.
- **Comment bodies** — Processed through `FilterPipeline` with format whitelisting (now fixed). Safe.
- **Block editor content** — Per-block-type sanitization with ammonia. Safe.

### References

- [Source: _bmad-output/planning-artifacts/epics.md — Story 27.1]
- [Source: crates/kernel/src/content/filter.rs — FilterPipeline implementation]
- [Source: crates/kernel/src/routes/helpers.rs — html_escape function]
- [Source: crates/kernel/src/theme/engine.rs — Tera autoescape configuration]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- **AC #1 verified:** Tera autoescape is ON globally via `Tera::new()` default. No `{% autoescape false %}` or `| raw` found.
- **AC #2 verified:** All 30+ `| safe` usages audited and categorized into 6 groups. All justified.
- **AC #3 fixed (Finding B):** Added `is_valid_attr_key()` to `get_extra_attrs()` in `render.rs` — validates attribute keys match `[a-zA-Z][a-zA-Z0-9-_]*`, skips invalid keys. Also escaped non-string attribute values.
- **AC #4 fixed (Finding C):** Added format whitelisting to `front.rs` at both `render_promoted_listing` body field and `render_item_fields` — rejects `full_html` and unknown formats.
- **AC #4 fixed (Finding D):** Added format whitelisting to all 4 comment render sites in `comment.rs`.
- **AC #5 verified:** All admin-entered content paths audited — all use Tera autoescape or FilterPipeline.
- **AC #6 documented:** 4 findings documented with severity ratings (2 MEDIUM, 2 LOW). No Critical/High.
- **AC #7 satisfied:** No Critical/High findings to fix. All MEDIUM findings fixed. All LOW findings fixed as defense-in-depth.
- **Finding A hardened:** Added `sanitize_snippet()` to `search.rs` — escapes all HTML then restores `<mark>` tags. Applied before template context.
- **Tests added:** 7 new unit tests (3 for attr key validation, 4 for snippet sanitization).

### File List

- `~ crates/kernel/src/content/filter.rs` (added `for_format_safe()` — single source of truth for format whitelisting, 4 tests)
- `~ crates/kernel/src/theme/engine.rs` (text_format filter uses `for_format_safe()`)
- `~ crates/kernel/src/theme/render.rs` (attr key validation, `process_value` uses `for_format_safe`, tag allowlist, class escaping, element_type escaping, 7 tests)
- `~ crates/kernel/src/routes/front.rs` (uses `for_format_safe` at 2 sites, field name escaping)
- `~ crates/kernel/src/routes/comment.rs` (`render_comment_body()` helper uses `for_format_safe`)
- `~ crates/kernel/src/routes/item.rs` (uses `for_format_safe` at 2 sites)
- `~ crates/kernel/src/routes/search.rs` (sanitize_snippet for HTML and JSON, item_type escaping, 4 tests)

## Senior Developer Review — Round 1

### Review Model Used

Claude Opus 4.6

### Review Findings

| ID | Severity | Location | Issue | Fix |
|----|----------|----------|-------|-----|
| H1 | HIGH | `render.rs:process_value()` | Format not whitelisted — plugin could request `full_html` to bypass sanitization | Added format whitelist matching front.rs/comment.rs pattern |
| M1 | MEDIUM | `search.rs:search_json()` | JSON API did not sanitize snippets; HTML endpoint did | Applied `sanitize_snippet()` to JSON results |
| M2 | MEDIUM | `render.rs:render_inline()` | Plugin-supplied `element_type` interpolated into HTML class without escaping | Added `html_escape()` for element_type |
| M3 | MEDIUM | `render.rs:render_markup()` | Plugin-supplied `tag` used directly as HTML tag name | Added `SAFE_TAGS` allowlist; unknown tags fall back to `span` |
| L1 | LOW | `front.rs:render_item_fields()` | JSONB field name used in class attribute without escaping | Added `html_escape()` for field name |
| L2 | LOW | `search.rs:render_fallback_search()` | `item_type` not escaped in fallback renderer | Added `html_escape()` for item_type |
| L3 | LOW | `comment.rs` (4 sites) | Format whitelist duplicated 4 times | Extracted `render_comment_body()` helper function |

### Review Verdict

All 7 findings fixed and verified.

## Senior Developer Review — Round 2 (Adversarial)

### Review Model Used

Claude Opus 4.6

### Review Findings

| ID | Severity | Location | Issue | Fix |
|----|----------|----------|-------|-----|
| R2-1 | CRITICAL | `engine.rs:73` | Tera `text_format` filter calls `for_format()` directly — bypasses whitelist | Changed to `for_format_safe()` |
| R2-2 | HIGH | `render.rs:classes_to_string()` | Class values from plugins not escaped — attribute injection | Added `html_escape()` to each class value |
| R2-3 | HIGH | `render.rs:render_inline()/render_container()` | Unescaped class string concatenated into class attribute | Fixed by escaping at source (R2-2) |
| R2-4 | MEDIUM | `render.rs:SAFE_TAGS` | `input`, `link`, `meta` enable clickjacking/CSS injection/redirects | Removed from allowlist |
| R2-5 | MEDIUM | 8 sites across 4 files | Format whitelist duplicated with no single source of truth | Added `FilterPipeline::for_format_safe()`, replaced all sites |
| R2-6 | MEDIUM | `render.rs:render_markup()` | No test coverage for tag allowlist | Added tests for unsafe/safe tag handling |
| R2-7 | LOW | `render.rs:process_value()` | Double `unwrap_or` was confusing | Simplified to single line using `for_format_safe()` |
| R2-8 | LOW | `comment.rs:render_comment_body()` | Took full `&Comment` for 2 fields | Kept as-is; coupling is acceptable for a private helper |
| R2-9 | LOW | `search.rs:sanitize_snippet()` | `<mark>` in indexed content restored as HTML tag | Documented as acceptable — `<mark>` is presentational only |
| R2-10 | LOW | `front.rs:render_promoted_listing()` | Manual HTML construction bypasses Tera autoescape | Documented as maintenance hazard — no active fix needed |

### Review Verdict

All actionable findings (R2-1 through R2-7) fixed. R2-8 through R2-10 documented as acceptable risk. `cargo fmt`, `cargo clippy`, and `cargo test --all` pass clean.
