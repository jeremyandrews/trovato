# Plugin Error Codes Reference

When a host function call fails, it returns a negative `i32` error code. This document explains each code and how to handle it.

The authoritative source is `crates/plugin-sdk/src/host_errors.rs`.

## General Host Errors

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -1 | `ERR_MEMORY_MISSING` | WASM module does not export `"memory"` | Plugin build issue — ensure the WASM target exports memory |
| -2 | `ERR_PARAM1_READ` | First parameter read failed (UTF-8 or out-of-bounds) | Plugin code bug — check string parameter encoding |
| -3 | `ERR_PARAM2_OR_OUTPUT` | Second parameter or output write failed | Buffer too small or out of bounds |
| -4 | `ERR_PARAM3_READ` | Third parameter read failed | Check extra parameters (DB params, etc.) |
| -10 | `ERR_NO_SERVICES` | Tap executed without request context | Only occurs in background taps without AppState |

## Database Errors

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -11 | `ERR_DDL_REJECTED` | SQL statement rejected (CREATE, DROP, ALTER, etc.) | Use `execute_raw()` for DML; `query_raw()` for SELECT only |
| -12 | `ERR_SQL_FAILED` | SQL execution failed (syntax, constraint, timeout) | Review query; check statement timeout (5s for plugins) |
| -13 | `ERR_SERIALIZE_FAILED` | Result serialization to JSON failed | Kernel bug — file issue |
| -14 | `ERR_PARAM_DESERIALIZE` | JSON parameter deserialization failed | Check parameter JSON format |
| -15 | `ERR_INVALID_IDENTIFIER` | Invalid table or column name | Names must match `[a-zA-Z_][a-zA-Z0-9_]*` |
| -82 | `ERR_DB_BATCH_TOO_LARGE` | `insert-many` was given more than 500 rows; nothing was written (added by B80) | Split the batch |

An array `where` on `update` or `delete` that is empty, names an unknown op, or has an empty `in` list is `-14`, as are `insert-many` rows that disagree on their columns.

## AI API Errors

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -20 | `ERR_AI_NO_PROVIDER` | No provider configured for this operation type | Admin must configure a provider in AI settings |
| -21 | `ERR_AI_REQUEST_FAILED` | HTTP request to AI provider failed | Check provider availability; retry later |
| -22 | `ERR_AI_RATE_LIMITED` | Rate limit exceeded (provider or local) | Wait and retry; check rate limit configuration |
| -23 | `ERR_AI_INVALID_REQUEST` | Malformed `AiRequest` JSON | Check AiRequest structure and message roles |
| -24 | `ERR_AI_AUTH_FAILED` | Provider returned 401/403 | Check API key configuration |
| -25 | `ERR_AI_PROVIDER_ERROR` | Provider returned non-2xx error | Check provider status; review request parameters |
| -26 | `ERR_AI_BUDGET_EXCEEDED` | Token budget exceeded for current period | Wait for budget reset or increase limit in admin |
| -27 | `ERR_AI_PERMISSION_DENIED` | User lacks AI permission | Grant `use ai` or operation-specific permission |

## HTTP API Errors

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -30 | `ERR_HTTP_REQUEST_FAILED` | Network/DNS/connection error | Check endpoint availability |
| -31 | `ERR_HTTP_TIMEOUT` | Request timed out (30s default) | Reduce response time or increase timeout |
| -32 | `ERR_HTTP_INVALID_URL` | URL malformed or blocked (SSRF prevention) | Use public HTTPS URLs only |
| -33 | `ERR_HTTP_RESPONSE_TOO_LARGE` | Response body exceeded buffer | Reduce response size |

The one-shot `http.request` (byte-identical, unchanged) uses `-30`..`-33` above. The
additive streaming trio `http.http-open` / `http.http-read` / `http.http-close`
(P11e / D-49, D-50) — for bodies too large to buffer whole in WASM — shares
`-30`..`-33` on open and adds:

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -37 | `ERR_HTTP_HANDLE_INVALID` | Handle unknown, already closed, or from another tap call (handles are Store-scoped) | Only read/close a handle from the same call that opened it |
| -38 | `ERR_HTTP_STREAM_READ_FAILED` | Network error reading the next body chunk | Transient — reopen and retry |
| -39 | `ERR_HTTP_TRANSFER_BUDGET` | Fetch exceeded the manifest-declared, kernel-capped total-transfer ceiling (preflight or mid-stream) | Raise `http_max_transfer` (≤ 16 MB) or fetch less |
| -40 | `ERR_HTTP_TOO_MANY_HANDLES` | This call already holds the max concurrent open handles | Close a handle before opening another |

Streaming semantics (D-49/D-50): `http-open` returns an opaque Store-scoped handle;
`http-read` returns ≤ 64 KB per call (the tap I/O buffer), `0` = EOF; `http-close`
releases the connection. The same SSRF fence as `request` applies on open. Total
bytes are bounded by the plugin's `http_max_transfer` capability (default 1 MB,
kernel max 16 MB); the old 60 s ceiling becomes a total-transfer wall-clock budget
plus a per-read timeout, and a background tap is additionally bounded by the 150 s
epoch (the epoch cuts CPU, the transfer budget cuts the wire). Inherited, unchanged
gaps: the SSRF fence is literal-string / IP-literal based — DNS-rebinding (TOCTOU)
and redirect re-validation remain out of scope and are not widened by this surface.

## Item API Errors

`get-item`, `save-item`, `delete-item`, `query-items` and, since B80,
`list-translations`, `save-translation` and `delete-translation` (SDK: `get_item`,
`save_item`, `delete_item`, `query_items`, `list_translations`,
`save_translation`, `delete_translation`). They also return the general codes
above and `-12`, `-13`, `-14` from the database table. A translation write
returns `-14` for a language the site does not have, or the item's own, and
`-62` for an item id that names no item.

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -60 | `ERR_ITEM_ACCESS_DENIED` | The user the plugin acts for may not make this write: no `edit` or `delete` access, no `create {type} content`, a field they may not edit, or a publish change without `publish content`. Also any call made while an access decision is in progress. Never returned by a read: a denied read is `null`, like a missing item | Do the work on behalf of a user who may, or from a background context with `item_background` |
| -61 | `ERR_ITEM_BACKGROUND_DENIED` | Called from cron or a queue worker without the `item_background` capability | Declare `item_background = true` in `[capabilities]` if the plugin needs kernel authority there |
| -62 | `ERR_ITEM_NOT_FOUND` | `delete-item` was given an id that names no item | Treat as already deleted if that is what you meant |
| -63 | `ERR_ITEM_WRITE_DEPTH` | A write made from inside item write taps nested 4 deep | A tap that writes items should not depend on a chain of other plugins' taps writing in turn |
| -64 | `ERR_ITEM_SERVICE_UNAVAILABLE` | The dispatch path carries no item service, so nothing was decided or written; the server log names the plugin and the call | A kernel wiring fault or a test harness that never bound one, not a plugin error: report it |

## Text Format Errors

Returned by `host::filter_text` (`trovato:kernel/text-format`, added by B71).

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -70 | `ERR_TEXT_FORMAT_NOT_PERMITTED` | The user the plugin acts for may not use this format | Filter with a format they may use; `plain_text` always works |
| -71 | `ERR_TEXT_FORMAT_UNKNOWN` | No such format | Use a format listed at `/admin/config/formats`; `plain_text` always exists |
| -72 | `ERR_TEXT_FORMAT_TOO_LARGE` | The filtered text is larger than the output buffer | Filter smaller pieces |

## Query Errors

`query.run` (added by B80). It also returns `-12` when a statement fails or runs
past the 5 second timeout, `-14` for JSON that is not a definition or options
object, `-16` for a table outside the plugin's allowlist, and `-61` for a content
read from cron or the queue without `item_background`.

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -80 | `ERR_QUERY_INVALID` | The definition is refused: `stage_aware = false` on content, an include, `semantic_similarity`, a plugin-table base joining content, a `url` filter other than `equals` or `in`, or anything the gather validator rejects | The host log names the reason; see `docs/design/plugin-query-surface.md` |
| -81 | `ERR_QUERY_BOUND` | More than 100 rows per call, 32 filters (grouped ones and `having` included), 8 sorts, 3 relationships, filter groups nested past 3, or more than 8 `group_by` columns or aggregates | Page with `page`, or split the query |

## Vocabulary Errors

Returned by `host::create_owned_vocabulary` and `host::list_owned_vocabularies`
(`trovato:kernel/vocabulary`, added by B108). They also return `-12` when the
write or read fails and `-14` for a request that is not valid JSON.

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -90 | `ERR_VOCABULARY_OWNER_UNDECLARED` | The owner type is not one this plugin declares under `[[vocabulary_owners]]` | Declare it in the manifest |
| -91 | `ERR_VOCABULARY_INVALID` | The id is not a machine name of at most 32 characters, the label is empty, or the owner id is empty or longer than 255 | Fix the request |
| -92 | `ERR_VOCABULARY_EXISTS` | A vocabulary with this id exists | Put the owner in the id, or list the owner's vocabularies first |

## User Directory Errors

Returned by `host::load_users` (`trovato:kernel/user-api` `load-users`, added by
B73).

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -73 | `ERR_USER_DIRECTORY_DENIED` | The plugin does not declare `user_directory = true` under `[capabilities]` | Declare it, if the plugin needs to read about users it is not serving |
| -74 | `ERR_USER_IDS_INVALID` | An id is not a user id (a UUID) | Pass user ids as the kernel gave them |
| -75 | `ERR_USER_IDS_TOO_MANY` | More than 100 ids in one call | Split the batch |

## Mail to Member Errors

Returned by `host::mail_send_to_user` (`trovato:kernel/mail` `send-to-user`,
added by B73), besides the `ERR_MAIL_*` codes `mail_send_to_site_contacts`
returns for a malformed message (-52), no SMTP host (-50), a failed delivery
(-54) and a spent bucket (-55).

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -56 | `ERR_MAIL_USERS_DENIED` | The plugin does not declare `mail_users = true` under `[capabilities]` | Declare it, if the plugin needs to mail members |
| -57 | `ERR_MAIL_RECIPIENT_UNAVAILABLE` | No such user, the user is blocked, or the user has no address (one code for all three) | Do not retry |
| -58 | `ERR_MAIL_OPTED_OUT` | The member unsubscribed from this plugin's mail | Do not retry; the member can resubscribe from their account page |

## Queue API Errors

`queue.push` (byte-identical, unchanged) returns `-1` memory missing, `-2`/`-3`
parameter read, `-4` malformed payload JSON, `-5` DB insert failed. The additive
`queue.enqueue` (P11d / D-48, carries `priority`/`delay`) adds:

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -34 | `ERR_QUEUE_INVALID_PAYLOAD` | `enqueue` payload was not valid JSON | Serialize a valid JSON value |
| -35 | `ERR_QUEUE_INVALID_OPTS` | `enqueue` opts were not a valid `{priority?, delay?}` object | Build opts with `QueueOptions` |
| -36 | `ERR_QUEUE_INSERT_FAILED` | `enqueue` DB insert failed | Transient — the job was not enqueued; retry |

See [`plugin-queue.md`](plugin-queue.md) for queue semantics (at-least-once delivery, backoff, dead-lettering).

## SDK-Side Errors

These are produced by SDK wrapper functions before/after the WASM boundary:

| Code | Constant | Meaning | Recovery |
|------|----------|---------|----------|
| -100 | `ERR_SDK_SERIALIZE` | JSON serialization failed before calling host | Plugin code bug — check serde derives |
| -101 | `ERR_SDK_UTF8` | UTF-8 decoding of response buffer failed | Kernel bug — file issue |
| -102 | `ERR_SDK_DESERIALIZE` | Response JSON deserialization failed | SDK version mismatch or kernel bug |
| -103 | `ERR_SDK_OUTPUT_BUFFER_EXCEEDED` | Result exceeded 256KB buffer limit | Add LIMIT to queries or paginate |

## Handling Errors in Plugins

```rust
use trovato_sdk::host::{query, log};
use trovato_sdk::host_errors;

let conferences = json!({
    "base_table": "item",
    "item_type": "conference",
    "sorts": [{"field": "created", "direction": "desc"}]
});
match query(&conferences, &json!({"per_page": 100})) {
    Ok(page) => {
        // page.items: what the viewer may see, each with its url
    }
    Err(host_errors::ERR_QUERY_BOUND) => {
        // Past a bound (100 rows a page, 32 filters, ...): page or narrow it
        log("warn", "my_plugin", "query past a bound");
    }
    Err(host_errors::ERR_QUERY_INVALID) => {
        // The definition was refused — log and return gracefully
        log("error", "my_plugin", "query definition refused");
    }
    Err(code) => {
        // Unknown error — log the code for debugging
        log("error", "my_plugin", &format!("host error: {code}"));
    }
}
```

### Best Practice

- Always match specific error codes you can handle, then fall through to a generic log
- Never panic on host errors — panics crash the WASM instance and disable the plugin
- Use `log()` to send errors to the kernel's tracing system for debugging
- For database queries, always use LIMIT to stay within the 256KB buffer
