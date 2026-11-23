Each JSON capability now has one path, and HTTP and MCP apply the same rules to
it.

`GET /api/v1/user/export` is gone. It duplicated `/user/data-export`, read the
models and raw SQL directly, gave administrators every account's data on the
`is_admin` flag rather than a permission, and had none of the one-an-hour
`data_export` limit. Nothing in the kernel called it; the account page's
download is the export. The item autocomplete reads its candidates through
`ItemService::title_prefix_candidates` instead of SQL in the route, and now
escapes a backslash in the prefix as well as `%` and `_`. The bearer-token
middleware and the MCP server resolve a token through one function,
`services::api_token::resolve`. Before this they each called the `ApiToken`
model on their own: the MCP server checked the account's status and recorded
the use inline, and the middleware did neither.

The HTTP item reads (`/api/v1/items`, `/api/v1/items/{type}`,
`/api/v1/item/{id}`) now require `access content`, as the MCP tools do. A
caller who is not the site administrator lists published items only, as
`list_items` lists them. Titles over `content::MAX_TITLE_LENGTH` (1000 bytes)
are refused on the HTTP item writes as on MCP. A search or autocomplete query
over `search::MAX_QUERY_LENGTH` (1000 bytes) is refused by the JSON API as by
the MCP `search` tool. The two limits are constants each side reads, no longer
a literal in the MCP crate.

The dead-letter queue's JSON moves from `/admin/queue/dlq` to
`/api/v1/queue/dlq`, listed in the OpenAPI document. It requires
`administer site` instead of the `is_admin` flag, and lists a page in the API's
one shape rather than the first 500 rows.

Content locking works end to end. `trovato_content_locking` is enabled by
default and its heartbeat and break routes had no client, because nothing ever
took a lock. Both item edit forms (`/admin/content/{id}/edit` and
`/item/{id}/edit`) now take the lock when they open. An editor who finds it
held is shown who holds it and when it lapses. A holder of `break content lock`
gets a button that breaks it. A save under another editor's live lock is
refused with 409, and a successful save releases the lock.
`static/js/content-lock.js` sends a heartbeat once a minute while the form is
open. The heartbeat and break answers carry a fresh CSRF token because tokens
are single use. The lock service now exists whenever the plugin is enabled,
not only when it was enabled at boot.
