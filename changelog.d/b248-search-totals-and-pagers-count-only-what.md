`/search` and `/api/v1/search` took their total from one SQL count before the
access check, then dropped the rows `check_access` denied from the page. A
visitor could read "12 results" above a page of 9 rows, or get a short page
with a pager offering more, which told them a hidden item matched. The count
and the page now share one `WHERE`, built to match what `check_access` shows
and chosen once per request by `ItemService::search_visibility`. Searching now
needs `access content` as well as `search content`. An author's own
unpublished items are included only with `view own unpublished content`,
`view own content` or `view any content`; a type-specific permission or a
plugin, such as the blog plugin for its authors, still opens them but no
longer puts them in search. Items on an internal stage are left out for
everyone but a superuser, who now also finds other authors' unpublished items.
`check_access` still runs on each rendered page and logs a warning if it
denies a row, which in ordinary operation happens only for an author's own
unpublished item that a plugin denies them. See `UPGRADING.md`.
