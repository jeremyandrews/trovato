# Story 4.4: Search Box Tile

Status: ready-for-dev

## Story

As a site visitor,
I want a search box in the header on every page,
So that I can search from anywhere on the site.

## Acceptance Criteria

1. Search form displayed in Header slot on all pages
2. Form posts to `/search?q={query}`
3. Submitting navigates to search results page

## Tasks / Subtasks

- [ ] Create `tile.search_box.yml` config — Header slot, no path restrictions (AC: #1)
- [ ] Create `templates/tiles/search-box.html` with search form (AC: #2, #3)
- [ ] Import config
- [ ] Verify search box appears on all pages

## Dev Notes

- Tile configured in Header slot with no visibility path restrictions = visible everywhere
- Form: simple GET form posting to `/search` with `q` parameter
- CSRF not needed for GET forms

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Step 6] — search box tile
