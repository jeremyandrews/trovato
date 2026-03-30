# Story 47.3: Verify Gather-as-JSON-API

Status: ready-for-dev

## Story

As an **API consumer**,
I want Gather queries available as JSON endpoints,
so that I can consume structured data from Gather queries without scraping HTML.

## Acceptance Criteria

1. Gather queries with `display.routes` configuration are accessible as JSON when requested with `Accept: application/json`
2. JSON response format: `{ "items": [...], "pager": { "current_page": N, "total_pages": N, "total_items": N }, "query": { "name": "..." } }`
3. Content negotiation: `Accept: text/html` (or default) returns the HTML page; `Accept: application/json` returns JSON
4. JSON response contains the same items as the HTML rendering (same filters, sort, pagination)
5. At least 2 integration tests: JSON response format validation, content negotiation between HTML and JSON

## Tasks / Subtasks

**Phase 1 — Spike (< 1 day):**
- [ ] Check whether Gather routes already support `Accept: application/json` by reading `crates/kernel/src/routes/gather_routes.rs` (AC: #1)
- [ ] Document findings: does it work, partially work, or not at all?

**Phase 2 — Implementation (if not already working):**
- [ ] Add `Accept` header parsing to Gather route handlers (AC: #3)
- [ ] Branch: if `application/json` preferred, serialize query results to JSON instead of rendering templates (AC: #1, #2)
- [ ] Build JSON response envelope: `{ items: [...], pager: {...}, query: {name: "..."} }` (AC: #2)
- [ ] Verify JSON items match HTML items — same query execution path, only the output format differs (AC: #4)

**Phase 3 — Verification and docs:**
- [ ] Write integration test: request Gather route with `Accept: application/json`, validate response structure (AC: #5)
- [ ] Write integration test: same URL returns HTML by default and JSON with accept header (AC: #5)
- [ ] Document the JSON API format for Gather queries

## Dev Notes

### Architecture

Content negotiation should be handled early in the Gather route handler. Check the `Accept` header: if it includes `application/json` (and prefers it over `text/html`), serialize the query results directly instead of passing them through the template engine.

The JSON format wraps items with pagination metadata. The `items` array should contain the same serialized item data that the template receives, ensuring parity between HTML and JSON outputs.

### Testing

- Set up a Gather query with known data. Request with `Accept: application/json`. Verify the response is valid JSON with `items`, `pager`, and `query` fields.
- Request the same URL with `Accept: text/html`. Verify an HTML response is returned.
- Verify the item count in JSON matches what the HTML page would show.

### References

- `crates/kernel/src/routes/gather_routes.rs` -- Gather route handlers
- `crates/kernel/src/gather/` -- Gather query execution
