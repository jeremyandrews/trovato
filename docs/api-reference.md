# Trovato CMS — API Reference

Trovato exposes a JSON API suitable for headless CMS usage. All endpoints
accept and return `application/json` unless otherwise noted.

## Conventions

- **One prefix.** Every kernel JSON endpoint is under `/api/v1` and answers
  with the `x-api-version: 1` header. The paths served before 1.x without the
  version (`/api/items`, `/api/search`, `/api/tokens`, ...) still answer, as
  deprecated aliases of the same handlers: each carries a `Deprecation: true`
  header, is marked `deprecated` in the OpenAPI document, and is removed at
  2.0. Plugin routes keep the paths their plugins declare.
- **One list shape.** Every list is a page:

  ```json
  {"data": [...], "total": 42, "page": 1, "per_page": 20}
  ```

  and takes `page` (from 1) and `per_page`; `limit` and `offset` are accepted
  as their aliases. `per_page` is clamped to each list's maximum. `total` is
  the size of the matching set; on a list an access check thins after the
  query, a page may hold fewer entries than `per_page`.
- **One error shape.** Every error is an `AppError` body (`message`, and
  `errors` for a validation failure).
- **One document.** `GET /api/v1/openapi.json` is the OpenAPI 3.0 document,
  generated from the router: every kernel route, every alias (deprecated) and
  every enabled plugin's `api` routes.

---

## Authentication

### Session Cookies

Login with username/password to receive a session cookie:

```
POST /user/login/json
Content-Type: application/json

{"username": "admin", "password": "secret"}
```

**Response (200):**
```json
{"success": true, "message": "Login successful"}
```

The `Set-Cookie` header contains the session ID. Include `credentials: "include"`
in fetch requests to send it cross-origin (requires specific CORS origins).

### Bearer Tokens

For external frontends, Bearer tokens avoid cookie/CORS complexity.

**Create a token** (requires session auth):

```
POST /api/v1/tokens
Content-Type: application/json
Cookie: <session>

{"name": "My Frontend"}
```

**Response (201):**
```json
{"id": "<uuid>", "name": "My Frontend", "token": "<raw-token>"}
```

Save the `token` value — it is shown only once.

**Use the token** on any request:

```
GET /api/v1/items
Authorization: Bearer <raw-token>
```

### Logout

```
GET /user/logout
```

---

## Common Patterns

### Pagination

List endpoints accept:

| Parameter  | Default | Range   | Description           |
|------------|--------:|--------:|-----------------------|
| `page`     |       1 |   1+    | 1-indexed page number |
| `per_page` |      20 | 1–100   | Results per page      |

**Note:** The Search API uses `limit` instead of `per_page` (see Search section).

Response includes a `pagination` object (or top-level fields):

```json
{
  "total": 42,
  "page": 1,
  "per_page": 20,
  "total_pages": 3
}
```

### Including Related Data

Some endpoints support `?include=author` (comma-separated). When included,
the response embeds related objects instead of just IDs.

### Error Responses

Every error from a kernel route is one JSON shape:

```json
{
  "code": "not_found",
  "message": "Comment not found",
  "request_id": "01a1fe41-419c-7007-8397-218151015cdd"
}
```

`code` is machine-readable (`bad_request`, `unauthorized`, `forbidden`,
`not_found`, `conflict`, `validation_failed`, `rate_limited`,
`internal_error`, ...), `message` is safe to show a person, and `request_id`
appears in the server log beside the failure. A validation failure adds
`details`, one `{field, code, message}` per field. A `429` carries
`Retry-After`.

`401` means nobody is signed in (no session, or the account is blocked or
gone); `403` means a signed-in caller may not do this.

Errors from a plugin's own API routes add `error`, the same string as
`message`, for clients written before the shapes were unified; it goes away in
2.0. The block editor's image upload keeps the Editor.js protocol's
`{"success": 0}`, which the editor reads.

### Timestamps

All timestamps are **Unix epoch seconds** (i64).

### IDs

Most resource IDs are UUIDv7. Categories and queries use string identifiers.

---

## Items

The item reads require `access content` (401 anonymous, 403 signed in), as
the MCP tools do. A caller who is not the site administrator lists published
items only, whatever `status` they ask for; an unpublished item they may see is
reached by its id. A title longer than 1000 bytes is refused on every write
path, and so is a search or autocomplete `q` longer than 1000 bytes.

### List Items

```
GET /api/v1/items?page=1&per_page=20&include=author
```

| Query Param | Type   | Description                     |
|-------------|--------|---------------------------------|
| `type`      | string | Filter by content type          |
| `status`    | i16    | Filter by status                |
| `author_id` | UUID   | Filter by author                |
| `page`      | int    | Page number (default 1)         |
| `per_page`  | int    | Results per page (default 20)   |
| `include`   | string | Comma-separated: `author`       |

**Response (200):**
```json
{
  "data": [
    {
      "id": "<uuid>",
      "type": "blog",
      "title": "Hello World",
      "status": 1,
      "author_id": "<uuid>",
      "author": {"id": "<uuid>", "name": "admin"},
      "created": 1708000000,
      "changed": 1708000000,
      "promote": 0,
      "sticky": 0,
      "fields": {},
      "stage_id": "live"
    }
  ],
  "total": 1,
  "page": 1,
  "per_page": 20
}
```

### Get Item

```
GET /api/v1/item/{id}?include=author
```

Returns a single item object (same shape as list items).

### List Content Types

```
GET /api/v1/content-types
```

Returns a page of content type machine names.

### List Items by Type

```
GET /api/v1/items/{type}
```

Returns a page of the items of the given content type (`id`, `title`,
`item_type`, `status`).

---

## Comments

### List Comments

```
GET /api/v1/item/{item_id}/comments?page=1&per_page=20&include=author
```

**Response (200):**
```json
{
  "data": [
    {
      "id": "<uuid>",
      "item_id": "<uuid>",
      "parent_id": null,
      "author_id": "<uuid>",
      "author": {"id": "<uuid>", "name": "admin"},
      "subject": "Great post!",
      "body": "Great post!",
      "body_html": "<p>Great post!</p>",
      "status": 1,
      "created": 1708000000,
      "changed": 1708000000,
      "depth": 0
    }
  ],
  "total": 1,
  "page": 1,
  "per_page": 20
}
```

### Create Comment

Requires authentication. `subject` is optional plain text (never a text
format), trimmed, capped at 255 characters, and absent when empty.

```
POST /api/v1/item/{item_id}/comments
Content-Type: application/json

{"subject": "Re: the article", "body": "Nice article!", "parent_id": null}
```

**Response (201):** Comment object.

### Get Comment

```
GET /api/v1/comment/{id}?include=author
```

### Update Comment

Requires authentication. Must be comment author or admin. `subject` absent
leaves it unchanged; present (including `""`) replaces it, independently of
`body` — a request can change one without the other.

```
PUT /api/v1/comment/{id}
Content-Type: application/json

{"body": "Updated text"}
```

### Delete Comment

Requires authentication. Must be comment author or admin.

```
DELETE /api/v1/comment/{id}
```

**Response:** `{"deleted": true}`

---

## Search

```
GET /api/v1/search?q=hello&page=1&per_page=10
```

| Query Param | Default | Range | Description        |
|-------------|--------:|------:|--------------------|
| `q`         |         |       | Search query       |
| `page`      |       1 |   1+  | Page number        |
| `per_page`  |      10 | 1–50  | Results per page (`limit` is an alias) |

The one JSON search: `/api/search` is its deprecated alias. An empty `q`
answers an empty page.

Searching needs both `search content` and `access content`, refused the same
way whichever is missing (the site administrator holds both without a
grant). The total, the page count and the pager are built from the same
rule as the page itself, so they no longer drift apart by stage, publish
status or a missing permission.

An item on an internal-visibility stage (an editorial workflow step, not a
staging server), published or not, or an item only a plugin's own access
check would allow, is left out of search for every role but the site
administrator (a superuser, `users.is_admin`), even where such a role could
open the item directly otherwise. An author's own unpublished item is
additionally a candidate only when they hold `view own unpublished content`,
and even then not if it sits on an internal-visibility stage.

**Response (200):**
```json
{
  "data": [
    {
      "id": "<uuid>",
      "type": "blog",
      "title": "Hello World",
      "rank": 0.85,
      "snippet": "...the <b>hello</b> world post...",
      "url": "/item/<uuid>"
    }
  ],
  "total": 1,
  "page": 1,
  "per_page": 10
}
```

---

## Gather (Queries)

A gather may declare the permission a viewer needs to run it, in its definition:
`"access": { "permission": "..." }`. Every endpoint below checks it before the
query runs: `/api/v1/queries` leaves out a gather the caller may not run, and the
other two answer 403 with a JSON `error` body (`Permission required: ...`).
Administrators pass. A lightweight-record gather (`record_type`) that declares
none requires `access content`. [Gathers that require a
permission](plugin-development.md#gathers-that-require-a-permission) covers the
HTML pages, route aliases, feeds and menu links.

### List Queries

```
GET /api/v1/queries
```

Lists the gathers the caller may run.

**Response (200):**
```json
{
  "data": [
    {
      "query_id": "blog_listing",
      "label": "Blog",
      "description": "Recent blog posts",
      "plugin": "core"
    }
  ],
  "total": 1,
  "page": 1,
  "per_page": 100
}
```

### Get Query Definition

```
GET /api/v1/query/{query_id}
```

Returns full query configuration including definition, display settings, filters,
and sorts.

### Execute Query

```
GET /api/v1/query/{query_id}/execute?page=1&stage=live
```

Exposed filters can be passed as query parameters. The gather sets the page
size; `page` picks the page.

**Response (200):**
```json
{
  "data": [ ... ],
  "total": 42,
  "page": 1,
  "per_page": 10
}
```

### Ad-Hoc Query

```
POST /api/v1/gather/query
Content-Type: application/json

{
  "definition": { "base_table": "item", "item_type": "blog", ... },
  "display": { "format": "list", "items_per_page": 10, ... },
  "page": 1,
  "stage": "live",
  "filters": {}
}
```

Returns the same response shape as Execute Query.

Requires `administer site`. Any other caller, including an anonymous one, gets a
403 with a JSON `error` body. The endpoint runs a definition the caller wrote, so
it takes the permission that guards the gather builder screens whose preview
calls it.

What a definition may read is checked on every gather, not only this one: named
gathers, plugin-seeded and config-imported gathers, and includes are held to the
same rules, and a definition that breaks them is refused before any SQL runs.

- `base_table` is `item`. A lightweight-record gather names its `record_type`
  instead, and reads that record's table.
- A relationship may join only `item`, `item_translation`, `category`,
  `category_tag`, `category_tag_hierarchy`, `url_alias` or `file_managed`.
- No definition may read account, credential, token, permission or secret
  tables (`users`, `roles`, `api_tokens`, `oauth_client`, `webhook` and the
  rest of the list in `gather_service.rs`), nor any `pg_` or `_sqlx` table,
  whatever it names them as.
- A field, filter or sort may not name a column called `pass`, `password`,
  `secret` or `token`, or one ending in `_hash`, `_secret` or `_token`.

---

## Categories & Tags

### Categories

| Method   | Path                  | Description        |
|----------|-----------------------|--------------------|
| `GET`    | `/api/v1/categories`     | List all           |
| `POST`   | `/api/v1/category`       | Create             |
| `GET`    | `/api/v1/category/{id}`  | Get one            |
| `PUT`    | `/api/v1/category/{id}`  | Update             |
| `DELETE` | `/api/v1/category/{id}`  | Delete (204)       |

Category object:
```json
{
  "id": "topics",
  "label": "Topics",
  "description": "Content topics",
  "hierarchy": 1,
  "weight": 0
}
```

### Tags

| Method   | Path                                | Description              |
|----------|-------------------------------------|--------------------------|
| `GET`    | `/api/v1/category/{cat_id}/tags`       | List tags in category    |
| `GET`    | `/api/v1/category/{cat_id}/roots`      | Root-level tags only     |
| `POST`   | `/api/v1/tag`                          | Create tag               |
| `GET`    | `/api/v1/tag/{id}`                     | Get tag                  |
| `PUT`    | `/api/v1/tag/{id}`                     | Update tag               |
| `DELETE` | `/api/v1/tag/{id}`                     | Delete tag (204)         |

Tag object:
```json
{
  "id": "<uuid>",
  "category_id": "topics",
  "label": "Rust",
  "description": null,
  "weight": 0,
  "created": 1708000000,
  "changed": 1708000000
}
```

### Tag Hierarchy

| Method | Path                        | Description                    |
|--------|-----------------------------|--------------------------------|
| `GET`  | `/api/v1/tag/{id}/parents`     | Parent tags                    |
| `PUT`  | `/api/v1/tag/{id}/parents`     | Set parents (`{parent_ids:[]}`) |
| `GET`  | `/api/v1/tag/{id}/children`    | Direct children                |
| `GET`  | `/api/v1/tag/{id}/ancestors`   | All ancestors with depth       |
| `GET`  | `/api/v1/tag/{id}/descendants` | All descendants with depth     |
| `GET`  | `/api/v1/tag/{id}/breadcrumb`  | Root-to-current path           |

Every `GET` above that answers more than one category or tag answers a page
(`data`, `total`, `page`, `per_page`; default 100, max 500), the breadcrumb and
the ancestors in their order.

---

## Batch Operations

### Create Batch

```
POST /api/v1/batch
Content-Type: application/json

{"operation_type": "export", "params": { ... }}
```

**Response (201):**
```json
{"id": "<uuid>", "status": "Pending"}
```

### Get Status

```
GET /api/v1/batch/{id}
```

**Response (200):**
```json
{
  "id": "<uuid>",
  "operation_type": "export",
  "status": "Running",
  "progress": {
    "total": 100,
    "processed": 42,
    "percentage": 42,
    "current_operation": "Exporting items..."
  },
  "result": null,
  "error": null,
  "created": 1708000000,
  "updated": 1708000100
}
```

Status values: `Pending`, `Running`, `Completed`, `Failed`, `Cancelled`.

### Cancel Batch

```
POST /api/v1/batch/{id}/cancel
```

Returns 409 if the operation already finished.

### Delete Batch

```
DELETE /api/v1/batch/{id}
```

**Response:** 204 No Content.

---

## API Tokens

All token management endpoints require an authenticated session.

A person manages their own tokens at `/user/tokens`, linked from their account
page (`/user/profile`). The page lists each token's name, when it was created,
last used and expires; creates one (a name, and an optional expiry in days) and
shows the raw token once; and revokes one. It calls the endpoints below with the
page's CSRF token, so it does nothing an API client cannot.

A token request is counted against its user's rate limit, at the count of the
user's role (see "Rate Limits" in `INSTALL.md`).

### Create Token

```
POST /api/v1/tokens
Content-Type: application/json

{"name": "My Frontend", "expires_in_days": 90}
```

| Field             | Type   | Required | Description                               |
|-------------------|--------|----------|-------------------------------------------|
| `name`            | string | yes      | Display name (1–255 chars)                |
| `expires_in_days` | int    | no       | Days until expiry. Omit for no expiration |

**Response (201):**
```json
{
  "id": "<uuid>",
  "name": "My Frontend",
  "token": "abcdef0123456789...",
  "expires_at": 1715000000
}
```

The `token` field is the raw Bearer token — store it securely, it cannot be
retrieved again. A maximum of 25 tokens per user is enforced.

### List Tokens

```
GET /api/v1/tokens
```

**Response (200):**
```json
{
  "data": [
    {
      "id": "<uuid>",
      "name": "My Frontend",
      "created": 1708000000,
      "last_used": 1708001000,
      "expires_at": 1715000000
    }
  ],
  "total": 1,
  "page": 1,
  "per_page": 100
}
```

### Revoke Token

```
DELETE /api/v1/tokens/{id}
```

**Response:** 204 No Content.

---

## AI Assistant

Configuring one thing by conversation. Every route belongs to the conversation's
owner and answers 404 to anybody else, administrators included. All four require
authentication and the assistant to be enabled at
`/admin/system/ai-assistant`.

### Get a conversation

```
GET /api/v1/assistant/{conversation_id}
```

Returns the transcript, the proposals, the conversation's limits, and whether it
is read-only. 404 unless the caller owns it.

### Send a message

```
POST /api/v1/assistant/{conversation_id}/message
X-CSRF-Token: <token>
Content-Type: application/json

{"message": "which devices have no owner?"}
```

Returns `text/event-stream`. Each event is a JSON object with a `type`:
`turn_start`, `tool_call`, `tool_result`, `proposal`, `assistant`, `note`,
`done`, `error`. The message is 1 to 4096 characters; the body is capped at
16 KiB. 409 when the conversation is read-only, 429 for the per-person rate
limit or an exhausted token budget (`"category": "token_budget"`).

### Apply or discard a proposal

```
POST /api/v1/assistant/{conversation_id}/proposals/{proposal_id}/apply
POST /api/v1/assistant/{conversation_id}/proposals/{proposal_id}/discard
```

Takes either an `X-CSRF-Token` header with an empty body, or a
`application/x-www-form-urlencoded` body with a `_token` field — the second is
the no-JavaScript path, and it answers 303 back to the conversation page rather
than JSON. Applying is the only thing anywhere that executes a write tool.
409 if the proposal was already resolved, 404 if it is somebody else's.

### Start over

```
POST /api/v1/assistant/{conversation_id}/reset
```

Closes the conversation, discards its outstanding proposals, and opens a fresh
one with a new snapshot. Same two token paths, same two response shapes.

---

## Autocomplete and media

```
GET /api/v1/items/autocomplete?type=article&q=ru&per_page=10
```

Suggestions for a reference field: a page (the first only; at most 50) of
`{"id", "title"}`. `type=category_term` answers category terms for a term
reference field and takes `item_type`, `field`, `item` and `owner`.

```
GET /api/v1/media/browse?page=1&per_page=24&type=image&q=logo&sort=newest
```

A page of the media library: `id`, `filename`, `mime_type`, `size`, `url`,
`thumbnail_url`, `created`. Requires a signed-in caller.

---

## Content locking

Served while `trovato_content_locking` is enabled (it is by default). The item
edit forms take the lock when they open, show another editor's lock, refuse a
save under it (409), and release it after a save; these three endpoints are
what the form's script calls.

```
POST /api/v1/lock/heartbeat
POST /api/v1/lock/break
POST /api/v1/lock/release
X-CSRF-Token: <token>
Content-Type: application/json

{"entity_type": "item", "entity_id": "<uuid>"}
```

Heartbeat extends the caller's own lock by 3 minutes (409 if they do not hold
it); break needs `break content lock`; release frees the caller's own lock at
once and is a no-op, not an error, if they do not hold it (it is what the form
beacons on `pagehide`, so it fires whether or not a save already released the
lock). All three answer `{"lock": "extended" | "broken" | "released",
"csrf_token": "<next token>"}`: CSRF tokens are single use, so the answer
carries the token for the next request.

`release` alone also accepts a form-urlencoded body with a `_token` field
instead of the header, because `navigator.sendBeacon` — which is how the form
sends this one, on `pagehide` — cannot set a custom header.

---

## Dead-letter queue

```
GET  /api/v1/queue/dlq?page=1&per_page=50
POST /api/v1/queue/dlq/{id}/requeue
POST /api/v1/queue/dlq/{id}/delete
```

Requires `administer site`; the two writes also `X-CSRF-Token`. See
[plugin-queue.md](plugin-queue.md#dead-letter-queue-dlq-observability).

---

## Health

```
GET /health
GET /health/live
GET /health/ready
```

**Response to an anonymous caller (200 or 503):**
```json
{
  "status": "healthy"
}
```

- `/health` answers 200 (`healthy`) when Postgres and Redis are reachable, 503
  (`unhealthy`) otherwise.
- `/health/live` answers 200 (`alive`) whenever the process answers, and
  checks nothing else.
- `/health/ready` answers 200 (`ready`) when Postgres and Redis are reachable,
  the schema is migrated, plugins are loaded and the server is not draining,
  503 (`not ready`) otherwise.

A caller on loopback with no forwarding header, or one sending
`Authorization: Bearer $HEALTH_TOKEN`, gets more: `/health` adds `postgres`,
`redis`, the pool statistics and the services report; `/health/ready` adds
`postgres`, `redis` and `draining`; `/health/live` adds nothing. See
[deployment/upgrades.md](deployment/upgrades.md#probes).


---

## CORS

Cross-origin requests are supported. Configure allowed origins via the
`CORS_ALLOWED_ORIGINS` environment variable (comma-separated). Default: `*`
(any origin, no credentials).

When specific origins are configured, `Access-Control-Allow-Credentials: true`
is set, enabling cookie-based auth cross-origin. Bearer tokens work regardless
of CORS origin configuration.

The session cookie's `SameSite` attribute defaults to `Strict`. Set
`COOKIE_SAME_SITE=lax` if you need cookie-based cross-origin authentication.
Bearer tokens (the recommended approach for external frontends) bypass cookies
entirely and work with any `SameSite` setting.
