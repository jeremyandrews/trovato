Every kernel JSON endpoint is served under `/api/v1`, every list answers in one
shape, and `/api/v1/openapi.json` is generated from the router. Fifteen paths
were versioned and about thirty-three were not; search was served twice, at
`/api/search` (`limit`, `{query, results, total_pages}`) and `/api/v1/search`
(`per_page`, Live only); the `x-api-version` header was on one router of many;
and the lists came in six shapes: `{items, pagination}`, `{data, total, page,
per_page}`, `{items, page_size}`, `{comments, total}`, gather's `{items,
total_pages, has_next}`, and bare arrays.

A route is now declared once, through `routes::api::ApiRoutes`, with its
handler and its OpenAPI description side by side. The same declaration mounts
it under `/api/v1` with the version header and lists it in the document; a
route served before the prefix existed also keeps its old `/api/...` path as an
alias of the same handler, answered with `Deprecation: true` and marked
`deprecated` in the document, until 2.0. `api_v1::kernel_api` is the one list
the router and the document are both built from, and a unit test refuses an
`/api` route registered any other way, so nothing can be served and not
listed. The integration test that requests every listed path now also requires
a kernel path to reach its route (the version or deprecation header only a
matched route gets), not just to avoid a 404. The hand-written list of eleven
kernel paths is gone; the document lists every kernel route, among them the
batch, token, category, tag, comment, gather, lock, assistant, media, page
builder and content type routes it was missing.

`/api/search` is folded into `/api/v1/search`: one handler, stage-aware as the
unversioned one was, returning `id`, `type`, `title`, `rank`, `snippet` and
`url` per result. Every list, over HTTP and MCP, is `crate::pagination::Page`,
`{data, total, page, per_page}`, and takes `page` and `per_page` with `limit`
and `offset` as aliases. A list the source does not page (categories, tags, a
comment thread, the gathers, the tokens, the content types) is paged in memory,
so it takes the same parameters. The MCP `search` tool now also takes `page`
and `per_page`, `list_items` also takes `limit` and `offset`, and every MCP list
tool answers in the same shape. The rate limiter files an alias in its
versioned route's bucket, so `/api/item/{id}/comments` and
`/api/v1/item/{id}/comments` share the comment limit.

The kernel's scripts and templates use the new paths and read `data`: the
comment form, the API token page, the block editor, the media picker, the
reference autocomplete, the gather builder preview (which read `rows`,
`results` or `data` and so showed nothing for the `items` it was sent) and the
page builder editor, rebuilt.
