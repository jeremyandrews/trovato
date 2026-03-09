# Story 4.3: Search JSON API

Status: ready-for-dev

## Story

As an API consumer,
I want to query `/api/search?q={query}` and receive JSON results with relevance scores,
So that I can integrate search into other applications.

## Acceptance Criteria

1. GET `/api/search?q=rust` returns JSON array of results
2. Each result: item ID, title, type, URL, snippet, relevance score
3. Results ordered by descending relevance
4. Empty query returns 400 error

## Tasks / Subtasks

- [ ] Implement/verify `/api/search` endpoint (AC: #1, #2, #3)
- [ ] Return 400 for empty query parameter (AC: #4)
- [ ] Integration test for API response structure

## Dev Notes

- API pattern: JSON response with `serde::Serialize` structs
- Existing search service returns scored results — wrap in API response
- Use `render_error()` for 400 on empty query

### References

- [Source: crates/kernel/src/routes/search.rs] — existing search routes
