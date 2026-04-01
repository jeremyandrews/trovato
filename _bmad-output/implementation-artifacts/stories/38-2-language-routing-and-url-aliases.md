# Story 38.2: Language Routing & URL Aliases

Status: done

## Story

As a **multilingual site visitor**,
I want the site to serve content in my preferred language based on URL prefix, session, or browser settings,
so that I can browse conferences in Italian or English seamlessly.

## Acceptance Criteria

1. Language negotiation middleware resolves active language per request using a priority chain: session override > URL prefix > Accept-Language header > default
2. URL prefix negotiator strips language prefix from URI (e.g., `/it/conferences` -> language "it", path `/conferences`)
3. Default language is excluded from URL prefix to prevent SEO duplicate content (`/en/about` does not match)
4. Accept-Language header parser supports quality values and returns highest-quality matching language
5. `ResolvedLanguage` stored in request extensions for downstream access
6. RTL language detection for proper `dir="rtl"` on HTML element (15 RTL language codes)
7. `LanguageNegotiator` trait enables pluggable negotiation strategies with priority ordering
8. Session language override via `SESSION_ACTIVE_LANGUAGE` key
9. `UrlPrefixNegotiator` uses O(1) HashSet lookup for language code matching

## Tasks / Subtasks

- [x] Define LanguageNegotiator trait with negotiate(), priority(), and negotiate_with_rewrite() (AC: #7)
- [x] Implement UrlPrefixNegotiator with HashSet-based O(1) lookup (AC: #2, #3, #9)
- [x] Implement AcceptLanguageNegotiator with quality value parsing (AC: #4)
- [x] Implement session-based language override negotiator (AC: #8)
- [x] Build language middleware that chains negotiators by priority (AC: #1)
- [x] Strip language prefix from URI and store ResolvedLanguage in extensions (AC: #2, #5)
- [x] Define RTL_LANGUAGES constant and text_direction_for_language() helper (AC: #6)
- [x] Exclude default language from URL prefix matching (AC: #3)

## Dev Notes

### Architecture

The language middleware (`middleware/language.rs`, 736 lines) uses the Strategy pattern with a chain of negotiators:

- **`UrlPrefixNegotiator`** (priority 100): Extracts language from the first path segment. Uses `HashSet` for O(1) lookup instead of linear scan. The default language is excluded to prevent duplicate URLs (SEO). The `negotiate_with_rewrite()` method returns both the language and the stripped path in a single pass for efficiency.
- **`AcceptLanguageNegotiator`** (priority 50): Parses the `Accept-Language` header with quality values (e.g., `it;q=0.9, en;q=0.8`). Sorts by quality descending and returns the first match against known languages.
- **Session negotiator** (priority 200): Reads `SESSION_ACTIVE_LANGUAGE` from the session store. Highest priority to honor explicit user preference.

The middleware chains negotiators sorted by priority (highest first), uses the first match, and injects `ResolvedLanguage` into request extensions. If URL prefix is detected, the URI is rewritten to the stripped path so downstream routes do not need language-aware path matching.

RTL support: `RTL_LANGUAGES` constant lists 15 RTL language codes. `text_direction_for_language()` checks the primary subtag against this list.

### Testing

- URL prefix extraction tested for various path patterns
- Accept-Language parsing tested with quality values
- Default language exclusion tested
- RTL detection tested for known RTL codes

### References

- `crates/kernel/src/middleware/language.rs` (736 lines) -- Full language negotiation middleware
