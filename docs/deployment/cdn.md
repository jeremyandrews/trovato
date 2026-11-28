# Behind a CDN (B303)

A Trovato site can sit behind a CDN: anonymous HTML pages say whether they
may be shared and for how long, carry a header listing the cache tags they
were built from, and every write that changes what a visitor sees tells the
CDN which tags to drop, through a purge plugin.

## The settings

Three environment settings, read once at startup:

| Setting | Default | What it does |
|---|---|---|
| `EDGE_CACHE_MAX_AGE` | `0` | Seconds a browser may cache a public page for (`max-age`). |
| `EDGE_CACHE_S_MAXAGE` | `0` | Seconds a shared cache (the CDN) may cache a public page for (`s-maxage`). |
| `EDGE_CACHE_TAG_HEADER` | `Surrogate-Key` | The header name the tags are sent in. |

A value that does not parse as a non-negative integer, or a tag header name
that is not a valid HTTP header token, refuses to start, naming the
variable, the same way `COOKIE_SECURE` does.

**The public half is off until `EDGE_CACHE_S_MAXAGE` is set above 0.** This
is deliberate: no existing site starts being cached by a CDN on upgrade. The
private half (below) is always on, because it only stops a shared cache from
storing a personal response, which no correct cache should store anyway.

Set the tag header to whatever your CDN's purge API expects:

- `Surrogate-Key` (the default): Fastly, Varnish.
- `Cache-Tag`: Cloudflare.
- `Edge-Cache-Tag`: Akamai.

## Which responses are public, and which are private

A response may be marked `Cache-Control: public, max-age=<EDGE_CACHE_MAX_AGE>,
s-maxage=<EDGE_CACHE_S_MAXAGE>` (plus `Vary: Cookie`, merged with any existing
`Vary`, and the tag header) only when **every one** of these holds:

- `EDGE_CACHE_S_MAXAGE` is set above 0;
- the method is `GET` or `HEAD`;
- the status is 200;
- the `Content-Type` is `text/html`;
- the request carries no session cookie and no `Authorization` header;
- the response sets no cookie;
- the page's language was decided by the URL alone (a language prefix like
  `/fr/...`, or the site's default language when the site has only one
  language), never by the visitor's session or an `Accept-Language` header.

That last rule is the one easy to miss. A CDN that ignores `Vary` for HTML
(Cloudflare does) would otherwise serve whichever visitor's negotiated
language cached first to every other visitor of the same URL. On a site with
more than one language, an **unprefixed** page (`/about`) is negotiated per
visitor and is never marked public; the same page under its language's
prefix (`/fr/about`) is, because that address means the same thing to every
visitor.

Anything that fails one of these, but was a request carrying a session
cookie or an `Authorization` header, was not `GET`/`HEAD`, or set a cookie,
gets `Cache-Control: private, no-store` instead, whatever
`EDGE_CACHE_S_MAXAGE` is set to. This half needs no setting: it is on by
default, because a signed-in page, an API response authenticated by a
bearer token, a form POST and anything that mints a session cookie (the
login page, for an anonymous visitor) must never sit in a shared cache.

Everything else (an anonymous 404, for example) gets no `Cache-Control`
from this layer at all, exactly as before this change. It is never marked
public.

A route that sets its own `Cache-Control` (a feed, a static file, a health
check) is left exactly as it answered; this layer never overwrites it.

## The tag vocabulary

Three kinds of tag, carried in the configured tag header, comma separated:

- `site`: every response this layer marks public carries it. Menus, tiles,
  aliases, site settings, the theme and the language all render on every
  page, so a write to any of them purges every cached page at once.
- `items`: carried by a response that ran a Gather query (a listing, the
  default front page when it is one).
- `item:<uuid>`: one per item loaded while serving the response (an item
  page, a tile that shows a specific item).

If the joined header would exceed 8 KiB, the `item:` tags are dropped and
`items`/`site` are kept: any write that would have purged an `item:` tag
also purges `items`, so nothing goes stale.

## What a write purges

| Write | Tags purged |
|---|---|
| Item create, update, delete, revision delete, translation write | `item:<uuid>`, `items` |
| Comment create, update, delete | `item:<the commented item's uuid>` |
| Tile create, update, delete | `site` |
| Menu link write, URL alias write, config import of either | `site` |
| Site setting write (`site_config`), except a runtime-state key (`installed`, the update check's own state) or a plugin variable (`plugin.<name>.<key>`) | `site` |
| Theme choice, language write, redirect write, Gather query definition write, category/term write, text format write, content type write | `site` |
| A role's permissions changed | `site` |
| Stage publish | everything (items, aliases and menus all moved onto Live at once) |
| Manual purge from the admin | whatever the operator named, or everything |

Notably **not** a purge trigger: a plugin variable write (often made from
`tap_cron`, with no visitor watching), and a command-line write (`config
import`, `trovato cache clear`): those wait out `s-maxage`, the same as any
other change a running server cannot see until its caches expire.

## The purge path

A write calls `edge_cache::purge(tags)` (or `purge_everything()` for a
stage publish) right after it commits. This only adds to a process-wide
pending set, cheap, and safe to call from anywhere. A background task
drains the set once a second: if at least one enabled plugin implements the
`tap_cache_purge` tap, it dispatches the tap once with
`{"tags": [...], "all": false}` (or `{"tags": [], "all": true}` when more
than 500 tags are pending, or an `purge_everything` was recorded). If no
plugin implements the tap, draining a pending set costs nothing beyond the
set insert that recorded it.

`tap_cache_purge` is a notification: a plugin that implements it only
enqueues the payload onto its own plugin queue (`queue_push`/`queue_enqueue`)
and returns. Delivery, retry and backoff are the plugin queue's job:
`tap_queue_worker` does the actual purge, and a transport failure or a
non-2xx answer from the CDN is retried with the queue's usual backoff and
dead-letter handling.

## The two shipped plugins

Both ship disabled (`default_enabled = false`); enable the one for your CDN
from the plugin admin.

### `trovato_cdn_purge_http`

A plain HTTP POST, for anything that accepts one (Varnish behind a small
script, a Fastly purge-by-tag webhook, an internal cache).

Variables (`/admin/extend` → the plugin's settings):

- `url` (required): where the purge is POSTed. Unset: the purge is dropped
  with a warning in the log, not retried.
- `secret` (optional): sent as `Authorization: Bearer <secret>`.

The POST body is the same JSON the tap received:
`{"tags": ["site", "item:0b6b3b8e-..."], "all": false}` or
`{"tags": [], "all": true}`, with `Content-Type: application/json`.

### `trovato_cdn_purge_cloudflare`

Calls Cloudflare's `purge_cache` API directly.

Variables:

- `zone_id` (required): the zone to purge.
- `api_token` (required): a token scoped to `Cache Purge` on that zone,
  sent as `Authorization: Bearer <api_token>`. Stored like any other secret
  variable: config export writes `env:TROVATO_SECRET_VARIABLE_PLUGIN_TROVATO_CDN_PURGE_CLOUDFLARE_API_TOKEN_VALUE`
  in its place (see `SECRET_VARIABLE_KEYS`), so the token never sits in a
  config file.

Named tags become `{"tags": [...]}` requests, chunked at 30 tags each (the
API's own limit); `all: true` becomes one `{"purge_everything": true}`
request. A response whose body is not `{"success": true, ...}` (including a
200 with `"success": false`) is treated as a failure and retried.

## Configuring Cloudflare

1. **Respect origin cache headers.** Cloudflare's default "Browser Cache
   TTL" and page rules can override what the origin sends; set the zone (or
   a page rule for HTML paths) to honor `Cache-Control` from the origin
   rather than a fixed Cloudflare-side TTL.
2. **Cache HTML.** Cloudflare's default cache rules skip `text/html` by
   default; add a cache rule (or a page rule) that caches HTML when
   `Cache-Control` allows it.
3. **Bypass when the session cookie is present.** Even though every
   personal response already answers `private, no-store`, add a cache rule
   that bypasses cache whenever the request carries an `id` cookie
   (`crate::session::SESSION_COOKIE_NAME`): defense in depth against a
   misconfigured rule elsewhere overriding origin headers.
4. **Cache tag header.** Set `EDGE_CACHE_TAG_HEADER=Cache-Tag`, Cloudflare's
   own header name, so `trovato_cdn_purge_cloudflare`'s purges name exactly
   what the edge cached under.
5. **Enable `trovato_cdn_purge_cloudflare`** and set its `zone_id` and
   `api_token` variables.

## Out of scope

No internal page cache (no in-process or Redis-held rendered page), no
`ETag`/`304` handling, no `stale-while-revalidate`. Purge by path or URL
pattern, and a `trovato cache purge` command, are not provided; the tag
vocabulary above is the whole purge surface. A multilingual site's
unprefixed pages stay uncacheable; turning off `Accept-Language`
negotiation so they could be is a later change. See `docs/BACKLOG.md`
BL-153 for what is still open.
