# Story 42.5: Gather Max Page Size

Status: ready-for-dev

## Story

As a kernel maintainer,
I want an upper bound on `per_page` in Gather queries,
so that clients cannot request unbounded result sets that exhaust server memory or degrade database performance.

## Acceptance Criteria

1. `GATHER_MAX_PAGE_SIZE` env var added to config with default value of `100`
2. Runtime `per_page` parameter clamped to `min(requested, max_page_size)`
3. `per_page` values of 0 or negative default to the Gather definition's `items_per_page`
4. API responses include `X-Max-Page-Size` header with the configured maximum
5. Admin UI pagination controls respect the max (cannot select a value above it)
6. The Gather definition's `items_per_page` is NOT clamped (only the runtime request parameter is bounded)

## Tasks / Subtasks

- [ ] Add `GATHER_MAX_PAGE_SIZE` to `crates/kernel/src/config.rs` with default `100` (AC: #1)
- [ ] Add clamping logic in Gather query execution: `per_page = min(requested, max_page_size)` (AC: #2)
- [ ] Add fallback logic: `per_page <= 0` falls back to definition's `items_per_page` (AC: #3)
- [ ] Add `X-Max-Page-Size` response header to Gather API responses (AC: #4)
- [ ] Update admin UI pagination controls to cap at `max_page_size` (AC: #5)
- [ ] Verify definition `items_per_page` is not subject to clamping (AC: #6)
- [ ] Write integration test: request with `per_page=500` is clamped to 100 (AC: #2)
- [ ] Write integration test: request with `per_page=0` uses definition default (AC: #3)
- [ ] Write integration test: `X-Max-Page-Size` header present in response (AC: #4)

## Dev Notes

### Architecture

The clamping should happen at the point where the runtime `per_page` parameter is resolved in the Gather query execution path, before the SQL query is built. This is a single check: `let per_page = if requested <= 0 { definition.items_per_page } else { requested.min(max_page_size) }`. The `X-Max-Page-Size` header informs API clients of the limit so they can adjust their pagination strategy.

### Security

- Without this bound, a malicious or buggy client can request `per_page=1000000`, causing the DB to return huge result sets and the server to serialize them into memory.
- The definition's `items_per_page` is trusted (set by the site builder in YAML config), so it is not clamped. Only untrusted runtime input (query params, API calls) is bounded.

### Testing

- Test with `GATHER_MAX_PAGE_SIZE=50` to verify the env var override works.
- Test edge cases: `per_page=-1`, `per_page=0`, `per_page=1`, `per_page=50` (at limit), `per_page=51` (over limit).
- Verify the response header value matches the configured max.

### References

- `crates/kernel/src/gather/` -- Gather query execution
- `crates/kernel/src/config.rs` -- config fields
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
