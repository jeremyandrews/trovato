A gather feed's RSS `<link>` and Atom `<link rel="alternate">` now point at the
URL alias of `/gather/{query_id}` when one exists and the gather has no
`canonical_url`, instead of `/gather/{query_id}` itself. The stock blog's
channel link is `/blog`, the address every reader and aggregator already uses,
not `/gather/blog_listing`.

The cause: the feed's channel link was built from the argument's own route,
else `canonical_url`, else `/gather/{query_id}` — it never consulted the
`url_alias` table at all. `trovato_blog`'s `blog_listing` gather has no
`canonical_url` (B127 left it unset: the gather's `/gather/blog_listing`
address already has a URL alias to `/blog`, and giving it a `canonical_url` of
`/blog` too would add a 301 redirect from `/gather/blog_listing` to `/blog`
beside the alias that already rewrites `/blog` to `/gather/blog_listing`), so
the whole blog feed linked the ugly address.

Resolving the alias is per request, not per restart, since a `url_alias` row
can be written at runtime. It is cached in `GatherService` and cleared the
moment an alias write bumps `CacheKind::Menus` — the same invalidation an alias
edit already triggers for navigation links — so a feed need not cost a query
on every request, and an alias written after startup reaches it without one.
The resolved link is now part of the feed's `ETag`, since it is part of the
document: previously an alias change that moved the channel link left the
`ETag` unchanged.

Argument feeds (`/blog/author/{user}/feed.xml`, `/blog/tag/{tag}/feed.xml`)
and any gather with a `canonical_url` are unaffected — both already named a
literal address and still take priority over the alias.
