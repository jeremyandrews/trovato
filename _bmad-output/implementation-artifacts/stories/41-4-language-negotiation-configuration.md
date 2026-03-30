# Story 41.4: Language Negotiation Configuration

Status: ready-for-dev

## Story

As a **site operator** running a multilingual site,
I want the language negotiation chain to be configurable,
So that I can control which negotiation methods are active and in what order.

## Acceptance Criteria

1. Language negotiation methods are configurable via `LANGUAGE_NEGOTIATION_METHODS` env var (comma-separated, ordered: e.g., `url_prefix,cookie,accept_header`)
2. Each method can be enabled/disabled independently
3. Default configuration: `url_prefix,accept_header` (cookie negotiation opt-in)
4. URL prefix negotiator's prefix list derived from enabled languages in the `language` table
5. Configuration changes take effect on next request (no server restart required -- language config is not startup-only)
6. Single-language sites (only one language in `language` table) skip negotiation entirely -- no URL prefix rewriting, no Accept-Language parsing

## Tasks / Subtasks

- [ ] Add `LANGUAGE_NEGOTIATION_METHODS` to `crates/kernel/src/config.rs` with default `"url_prefix,accept_header"` (AC: #1, #3)
- [ ] Parse comma-separated methods into ordered list of enum variants (AC: #1, #2)
- [ ] Modify `crates/kernel/src/middleware/language.rs` to read negotiation config per-request instead of using hardcoded chain (AC: #1, #5)
- [ ] Add early-return optimization: if only one language in `language` table, skip all negotiation and use that language (AC: #6)
- [ ] Verify URL prefix list is derived from `language` table enabled entries (AC: #4)
- [ ] Add config validation: warn on unrecognized method names, ignore gracefully (AC: #2)
- [ ] Add unit test: default config produces `[UrlPrefix, AcceptHeader]` (AC: #3)
- [ ] Add unit test: custom config `"accept_header,cookie"` produces correct chain (AC: #1)
- [ ] Add unit test: single-language site skips negotiation (AC: #6)
- [ ] Add unit test: empty/invalid config falls back to default (AC: #2)
- [ ] Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`

## Dev Notes

### Architecture

The middleware at `crates/kernel/src/middleware/language.rs` already has `UrlPrefixNegotiator` and `AcceptLanguageNegotiator` implementations. This story wires them to configuration instead of hardcoding the chain.

Recognized method names for the env var:
- `url_prefix` -- `UrlPrefixNegotiator`
- `cookie` -- cookie-based negotiation (new, opt-in)
- `accept_header` -- `AcceptLanguageNegotiator`

The single-language optimization is important: most Trovato sites are monolingual. They should not pay the cost of language negotiation on every request.

Config is read per-request (or cached with short TTL) so that changes take effect without restart. The language table query for prefix list can use the existing `LocaleService` cache.

### Security

No security implications. Language negotiation is purely presentational.

### Testing

- Unit tests for config parsing and chain construction
- Integration test verifying single-language optimization bypasses negotiation
- Test that unknown method names are logged and ignored

### References

- `crates/kernel/src/config.rs` -- existing env var configuration
- `crates/kernel/src/middleware/language.rs` -- existing negotiation middleware
- [Epic 41 source: docs/ritrovo/epic-11-i18n.md]
