# Story 41.2: Locale-Aware Date Formatting

Status: ready-for-dev

## Story

As a **site visitor** reading content in my language,
I want dates formatted according to my locale,
So that "March 30, 2026" appears as "30. Marz 2026" in German or "2026年3月30日" in Japanese.

## Acceptance Criteria

1. `format_date` Tera filter accepts an optional `trovato_locale` parameter: `{{ date | format_date(locale="de") }}`
2. When `trovato_locale` is omitted, defaults to `active_language` from template context (not hardcoded "en")
3. Supports at least 14 languages: en, de, fr, es, ja, zh, ar, he, pt, it, nl, ko, ru, pl
4. Date format patterns per locale stored in a compile-time lookup (not a runtime config table -- these are stable conventions)
5. `format_date` also accepts an optional `format` parameter for custom patterns: `{{ date | format_date(format="%Y-%m-%d") }}`
6. When both `trovato_locale` and `format` are provided, `format` takes precedence (explicit format overrides locale default)
7. Existing tutorial code using `format_date` without parameters continues to work (defaults to active language)
8. At least 3 locale formats tested (en, de, ja)

## Tasks / Subtasks

- [ ] Define locale-to-pattern lookup table as a `phf::Map` or `HashMap<&str, &str>` constant (AC: #3, #4)
  - [ ] en: `"%B %-d, %Y"`
  - [ ] de: `"%-d. %B %Y"`
  - [ ] fr: `"%-d %B %Y"`
  - [ ] es: `"%-d de %B de %Y"`
  - [ ] ja: `"%Y年%-m月%-d日"`
  - [ ] zh: `"%Y年%-m月%-d日"`
  - [ ] ar: `"%-d %B %Y"`
  - [ ] he: `"%-d %B %Y"`
  - [ ] pt: `"%-d de %B de %Y"`
  - [ ] it: `"%-d %B %Y"`
  - [ ] nl: `"%-d %B %Y"`
  - [ ] ko: `"%Y년 %-m월 %-d일"`
  - [ ] ru: `"%-d %B %Y"`
  - [ ] pl: `"%-d %B %Y"`
- [ ] Modify `format_date` filter in `crates/kernel/src/theme/engine.rs` to accept optional `trovato_locale` param (AC: #1)
- [ ] Modify `format_date` filter to read `active_language` from Tera context as fallback when `trovato_locale` not provided (AC: #2)
- [ ] Modify `format_date` filter to accept optional `format` param (AC: #5)
- [ ] Implement precedence: explicit `format` > locale pattern > "en" default (AC: #6)
- [ ] Verify existing `format_date` calls without params still work (AC: #7)
- [ ] Add tests for en, de, ja locale formatting (AC: #8)
- [ ] Add test for `format` param overriding locale (AC: #6)
- [ ] Add test for missing locale falling back to active_language (AC: #2)
- [ ] Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`

## Dev Notes

### Architecture

The `format_date` filter in `crates/kernel/src/theme/engine.rs` currently uses a hardcoded `"%B %-d, %Y"` pattern. This change makes it locale-aware via a compile-time lookup table.

Resolution order for the date pattern:
1. Explicit `format` parameter (if provided) -- highest priority
2. Locale pattern from lookup table (using explicit `trovato_locale` param or `active_language` from context)
3. English pattern `"%B %-d, %Y"` as ultimate fallback

Note: `chrono`'s `%B` (month name) renders in English regardless of locale. For non-Latin month names (Japanese, Chinese, Korean, Arabic, Hebrew, Russian), the patterns use numeric month (`%-m`) with locale-specific delimiters/suffixes. Full ICU/CLDR localized month names are deferred to a future plugin.

### Testing

- Unit tests for at least en, de, ja patterns
- Test explicit `format` overrides locale
- Test fallback chain: no params -> active_language -> "en"
- Verify existing template usage is unaffected

### References

- `crates/kernel/src/theme/engine.rs` -- format_date filter registration
- [Epic 41 source: docs/ritrovo/epic-11-i18n.md]
