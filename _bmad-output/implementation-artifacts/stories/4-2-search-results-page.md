# Story 4.2: Search Results Page

Status: ready-for-dev

## Story

As a site visitor,
I want to search for conferences and see relevant results with snippets,
So that I can quickly find conferences matching my interests.

## Acceptance Criteria

1. Results at `/search?q=rust` ordered by relevance
2. Each result shows highlighted snippet with search term bolded
3. Each result shows content type badge
4. Results paginated with next/previous controls
5. Empty state message for no results
6. Special characters handled safely (no SQL injection, HTML escaped)

## Tasks / Subtasks

- [ ] Verify/create `templates/search.html` with result display (AC: #1-#5)
- [ ] Implement snippet highlighting with `ts_headline()` (AC: #2)
- [ ] Add content type badges (AC: #3)
- [ ] Add pagination controls (AC: #4)
- [ ] Empty state: "No results found for '{query}'" (AC: #5)
- [ ] Verify query safety (AC: #6)

## Dev Notes

- Search route: `crates/kernel/src/routes/search.rs`
- Use `ts_headline()` for snippet generation with highlighted terms
- HTML-escape query in display — use `html_escape()` from `crate::routes::helpers`
- Pagination: reuse existing pager patterns from Gather

### References

- [Source: docs/design/search-architecture.md]
- [Source: crates/kernel/src/routes/search.rs]
