The front page renders any configured path at `/`, with no redirect. A gather
listing, a plugin route or an alias set as `site_front_page` used to answer `/`
with a 307 to itself (only an `/item/{uuid}` was rendered inline, by a second
copy of the item route's rendering), so a site whose front page was a listing
showed its internal path in the address bar. `/` is now rewritten to the
configured path before routing, the way Drupal's `system.site:page.front`
works, and served by the same router, middleware and handler as that path:
the same access checks (an anonymous visitor who may not see the target gets
its login redirect or 403), cache headers and query string (`/?page=2` is page
two of a gather front page). `/{lang}` is the front page in that language. The
page carries `<link rel="canonical">` to `/`, and an item front page offers its
translations at `/` and `/{lang}/` as before. A target the site answers 404 for
(a deleted item) falls back to the promoted listing with one log warning. The
path is cached in memory under a new `site_config` cache generation, so `/`
costs no extra database query; the site settings form refuses a path that does
not resolve. A theme's `page--front.html` now applies to the default listing
only: a configured front page uses its own path's layout.
