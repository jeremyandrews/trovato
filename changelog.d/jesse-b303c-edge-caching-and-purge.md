A Trovato site can now sit behind a CDN. Pages never said whether they could be
shared and the kernel never recorded what a page was built from, so a CDN
could neither cache anonymous content safely nor be told what to drop when it
changed. An anonymous, cookie-free `GET`/`HEAD` response that answers 200 with
HTML, in a language the URL alone decided, may now carry `Cache-Control:
public, max-age=<EDGE_CACHE_MAX_AGE>, s-maxage=<EDGE_CACHE_S_MAXAGE>` (both 0
by default, so no existing site starts being cached on upgrade), `Vary:
Cookie` and a tag header (`EDGE_CACHE_TAG_HEADER`, default `Surrogate-Key`)
naming the item, gather and site-wide tags it was built from; every other
response, including any that sets a cookie, gets `Cache-Control: private,
no-store`. Every write that changes what a visitor sees now records which
tags changed, drained once a second into one `tap_cache_purge` dispatch to
whichever purge plugin is enabled: the shipped `trovato_cdn_purge_http` (a
plain HTTP POST) and `trovato_cdn_purge_cloudflare` (Cloudflare's
`purge_cache` API). The performance page gained a "Purge the CDN" section for
a manual purge, by tag or everything. See `docs/deployment/cdn.md`.
