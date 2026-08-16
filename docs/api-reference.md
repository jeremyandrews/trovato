# Trovato CMS — API Reference

Trovato exposes a JSON API suitable for headless CMS usage. All endpoints
accept and return `application/json` unless otherwise noted.

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
POST /api/tokens
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
GET /api/items
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

All errors return JSON:

```json
{"error": "Description of the problem"}
```

Standard HTTP status codes: 400 Bad Request, 401 Unauthorized, 403 Forbidden,
404 Not Found, 409 Conflict, 500 Internal Server Error.

### Timestamps

All timestamps are **Unix epoch seconds** (i64).

### IDs

Most resource IDs are UUIDv7. Categories and queries use string identifiers.

---

## Items

### List Items

```
GET /api/items?page=1&per_page=20&include=author
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
  "items": [
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
  "pagination": {
    "total": 1,
    "page": 1,
    "per_page": 20,
    "total_pages": 1
  }
}
```

### Get Item

```
GET /api/item/{id}?include=author
```

Returns a single item object (same shape as list items).

### List Content Types

```
GET /api/content-types
```

Returns an array of content type machine names.

### List Items by Type

```
GET /api/items/{type}
```

Returns all items of the given content type.

---

## Comments

### List Comments

```
GET /api/item/{item_id}/comments?page=1&per_page=20&include=author
```

**Response (200):**
```json
{
  "comments": [
    {
      "id": "<uuid>",
      "item_id": "<uuid>",
      "parent_id": null,
      "author_id": "<uuid>",
      "author": {"id": "<uuid>", "name": "admin"},
      "body": "Great post!",
      "body_html": "<p>Great post!</p>",
      "status": 1,
      "created": 1708000000,
      "changed": 1708000000,
      "depth": 0
    }
  ],
  "total": 1
}
```

### Create Comment

Requires authentication.

```
POST /api/item/{item_id}/comments
Content-Type: application/json

{"body": "Nice article!", "parent_id": null}
```

**Response (201):** Comment object.

### Get Comment

```
GET /api/comment/{id}?include=author
```

### Update Comment

Requires authentication. Must be comment author or admin.

```
PUT /api/comment/{id}
Content-Type: application/json

{"body": "Updated text"}
```

### Delete Comment

Requires authentication. Must be comment author or admin.

```
DELETE /api/comment/{id}
```

**Response:** `{"deleted": true}`

---

## Search

```
GET /api/search?q=hello&page=1&limit=10
```

| Query Param | Default | Range | Description        |
|-------------|--------:|------:|--------------------|
| `q`         |         |       | Search query       |
| `page`      |       1 |   1+  | Page number        |
| `limit`     |      10 | 1–50  | Results per page   |

**Response (200):**
```json
{
  "query": "hello",
  "results": [
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
  "limit": 10,
  "total_pages": 1
}
```

---

## Gather (Queries)

### List Queries

```
GET /api/queries
```

**Response (200):**
```json
[
  {
    "query_id": "blog_listing",
    "label": "Blog",
    "description": "Recent blog posts",
    "plugin": "core"
  }
]
```

### Get Query Definition

```
GET /api/query/{query_id}
```

Returns full query configuration including definition, display settings, filters,
and sorts.

### Execute Query

```
GET /api/query/{query_id}/execute?page=1&stage=live
```

Exposed filters can be passed as query parameters.

**Response (200):**
```json
{
  "items": [ ... ],
  "total": 42,
  "page": 1,
  "per_page": 10,
  "total_pages": 5,
  "has_next": true,
  "has_prev": false
}
```

### Ad-Hoc Query

```
POST /api/gather/query
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

---

## Categories & Tags

### Categories

| Method   | Path                  | Description        |
|----------|-----------------------|--------------------|
| `GET`    | `/api/categories`     | List all           |
| `POST`   | `/api/category`       | Create             |
| `GET`    | `/api/category/{id}`  | Get one            |
| `PUT`    | `/api/category/{id}`  | Update             |
| `DELETE` | `/api/category/{id}`  | Delete (204)       |

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
| `GET`    | `/api/category/{cat_id}/tags`       | List tags in category    |
| `GET`    | `/api/category/{cat_id}/roots`      | Root-level tags only     |
| `POST`   | `/api/tag`                          | Create tag               |
| `GET`    | `/api/tag/{id}`                     | Get tag                  |
| `PUT`    | `/api/tag/{id}`                     | Update tag               |
| `DELETE` | `/api/tag/{id}`                     | Delete tag (204)         |

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
| `GET`  | `/api/tag/{id}/parents`     | Parent tags                    |
| `PUT`  | `/api/tag/{id}/parents`     | Set parents (`{parent_ids:[]}`) |
| `GET`  | `/api/tag/{id}/children`    | Direct children                |
| `GET`  | `/api/tag/{id}/ancestors`   | All ancestors with depth       |
| `GET`  | `/api/tag/{id}/descendants` | All descendants with depth     |
| `GET`  | `/api/tag/{id}/breadcrumb`  | Root-to-current path           |

---

## Batch Operations

### Create Batch

```
POST /api/batch
Content-Type: application/json

{"operation_type": "export", "params": { ... }}
```

**Response (201):**
```json
{"id": "<uuid>", "status": "Pending"}
```

### Get Status

```
GET /api/batch/{id}
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
POST /api/batch/{id}/cancel
```

Returns 409 if the operation already finished.

### Delete Batch

```
DELETE /api/batch/{id}
```

**Response:** 204 No Content.

---

## API Tokens

All token management endpoints require an authenticated session.

### Create Token

```
POST /api/tokens
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
GET /api/tokens
```

**Response (200):**
```json
[
  {
    "id": "<uuid>",
    "name": "My Frontend",
    "created": 1708000000,
    "last_used": 1708001000,
    "expires_at": 1715000000
  }
]
```

### Revoke Token

```
DELETE /api/tokens/{id}
```

**Response:** 204 No Content.

---

## Health

```
GET /health
```

**Response (200 or 503):**
```json
{
  "status": "healthy",
  "postgres": true,
  "redis": true
}
```

Returns 200 when both backends are reachable, 503 otherwise.

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
