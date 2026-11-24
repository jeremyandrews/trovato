//! WASM Host Function Error Codes
//!
//! All Trovato WASM host functions follow a standard error code convention
//! for their `i32` (or `i64`) return values. Negative values indicate errors;
//! non-negative values indicate success.
//!
//! Use the constants below instead of raw integer literals when implementing
//! or consuming host functions.
//!
//! # Standard Error Codes
//!
//! | Code | Constant | Meaning |
//! |------|----------|---------|
//! | `-1` | [`ERR_MEMORY_MISSING`] | WASM module does not export `"memory"` |
//! | `-2` | [`ERR_PARAM1_READ`] | First parameter read failed (UTF-8 / OOB) |
//! | `-3` | [`ERR_PARAM2_OR_OUTPUT`] | Second param or output write failed |
//! | `-4` | [`ERR_PARAM3_READ`] | Third parameter read failed (DB extra params) |
//! | `≥ 0` | — | Success: bytes written, rows affected, or boolean flag |
//!
//! # Per-API Details
//!
//! ## Database (`trovato:db/*`)
//!
//! Structured calls (`select`/`insert`/`update`/`delete`) enforce the plugin's
//! effective table allowlist (WASM-2 / D-19): `-16` ([`ERR_TABLE_NOT_DECLARED`])
//! if the target table is neither migration-owned nor listed in the manifest
//! `db_tables`. The raw calls (`query-raw`/`execute-raw`) require the declared
//! `raw_sql` capability: `-17` ([`ERR_RAW_SQL_NOT_DECLARED`]) otherwise.
//!
//! - **`select(query_ptr, query_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: query read failed, `-3`: output write failed
//!   - `-16`: target table not in the plugin's effective allowlist
//!   - `≥ 0`: bytes written to output buffer (JSON array of rows)
//!
//! - **`query-raw(sql_ptr, sql_len, params_ptr, params_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: SQL read failed, `-3`: params read failed,
//!     `-4`: output write failed
//!   - `≥ 0`: bytes written to output buffer
//!
//! - **`insert(table_ptr, table_len, data_ptr, data_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: table name read failed, `-3`: data read failed,
//!     `-4`: output write failed
//!   - `≥ 0`: bytes written (JSON of inserted row)
//!
//! - **`update(table_ptr, table_len, data_ptr, data_len, where_ptr, where_len) → i64`**
//!   - `-1`: memory missing, `-2`: table read failed, `-3`: data read failed,
//!     `-4`: where-clause read failed
//!   - `≥ 0`: rows affected
//!
//! - **`delete(table_ptr, table_len, where_ptr, where_len) → i64`**
//!   - `-1`: memory missing, `-2`: table read failed, `-3`: where-clause read failed
//!   - `≥ 0`: rows affected
//!
//!   `update` and `delete` take `where` as a map (equality) or, since B80, an
//!   array of `{"field", "op", "value"}` conditions, ANDed. An empty `where`,
//!   an unknown op or an empty `in` list is `-14`: none may mean every row.
//!
//! - **`upsert(table_ptr, table_len, data_ptr, data_len, conflict_ptr, conflict_len, out_ptr, out_max_len) → i32`**
//!   (B80) `-16` as above; `-14` for a malformed conflict spec; `≥ 0`: bytes
//!   written (a JSON array holding the row)
//!
//! - **`insert-many(table_ptr, table_len, rows_ptr, rows_len) → i64`** (B80)
//!   - `-82`: more than 500 rows ([`ERR_DB_BATCH_TOO_LARGE`]), nothing written
//!   - `-14`: no rows, or rows that disagree on their columns
//!   - `≥ 0`: rows inserted, all of them in one statement
//!
//! - **`execute-raw(sql_ptr, sql_len, params_ptr, params_len) → i64`**
//!   - `-1`: memory missing, `-2`: SQL read failed, `-3`: params read failed
//!   - `≥ 0`: rows affected
//!
//! ## Item API (`trovato:item-api/*`)
//!
//! Every call acts as the user the plugin is handling a request for; from cron
//! or the queue worker it needs the `item_background` capability. Writes go
//! through the kernel's item service, so they fire the item write taps.
//!
//! - **`get-item(id_ptr, id_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: ID read failed or not a UUID,
//!     `-3`: output write failed, `-10`: no services, `-12`: database error,
//!     `-13`: serialization failed, `-60`: called inside an access decision,
//!     `-61`: background call without `item_background`, `-64`: no item
//!     service on this dispatch path
//!   - `≥ 0`: bytes written: the item's JSON, or `null` when it is missing **or**
//!     the user may not view it (the two are indistinguishable on purpose)
//!
//! - **`save-item(item_ptr, item_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: item JSON read failed, `-3`: output write
//!     failed, `-10`: no services, `-12`: database error, `-13`: serialization
//!     failed, `-14`: not a JSON object, no `type` on a create, or a `language`
//!     the site does not have, `-60`: refused (no `edit` access, no
//!     `create {type} content`, a field the user may not edit, a publish change
//!     without `publish content`, or called inside an access decision),
//!     `-61`: background call without `item_background`, `-63`: nested too
//!     deep, `-64`: no item service on this dispatch path
//!   - `≥ 0`: bytes written: the saved item's JSON, or `null` for an update of
//!     an id that names no item
//!
//! - **`delete-item(id_ptr, id_len) → i32`**
//!   - `-1`: memory missing, `-2`: ID read failed or not a UUID, `-10`: no
//!     services, `-12`: database error, `-60`: refused, `-61`: background call
//!     without `item_background`, `-62`: no such item, `-63`: nested too deep,
//!     `-64`: no item service on this dispatch path
//!   - `0`: deleted
//!
//! - **`query-items(query_ptr, query_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: query JSON read failed, `-3`: output write
//!     failed, `-10`: no services, `-12`: database error, `-13`: serialization
//!     failed, `-14`: not JSON, `-60`: called inside an access decision,
//!     `-61`: background call without `item_background`, `-64`: no item
//!     service on this dispatch path
//!   - `≥ 0`: bytes written (JSON array of the items the user may view)
//!
//! ## Request Context (`trovato:request-context/*`)
//!
//! - **`get(key_ptr, key_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing or key not found
//!   - `≥ 0`: bytes written
//!
//! - **`set(key_ptr, key_len, value_ptr, value_len) → void`**
//!   - Silent no-op on memory or read failure
//!
//! ## Cache API (`trovato:cache-api/*`)
//!
//! - **`get(bin_ptr, bin_len, key_ptr, key_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing or cache miss
//!   - `≥ 0`: bytes written
//!
//! - **`set(…) → void`** / **`invalidate-tag(…) → void`**
//!   - Silent no-op on memory or read failure
//!
//! ## User API (`trovato:user-api/*`)
//!
//! - **`current-user-id(out_ptr, out_max_len) → i32`**
//!   - `0`: memory missing or no current user
//!   - `> 0`: bytes written (user ID string)
//!
//! - **`current-user-has-permission(perm_ptr, perm_len) → i32`**
//!   - `0`: memory error, read failure, or permission denied
//!   - `1`: permission granted
//!
//! ## Variables (`trovato:variables/*`)
//!
//! - **`get(name_ptr, name_len, default_ptr, default_len, out_ptr, out_max_len) → i32`**
//!   - `0`: memory missing (returns default length otherwise)
//!   - `> 0`: bytes written
//!
//! - **`set(name_ptr, name_len, value_ptr, value_len) → i32`**
//!   - `-1`: memory missing
//!   - `0`: success
//!
//! ## Logging (`trovato:logging/*`)
//!
//! - **`log(level_ptr, level_len, plugin_ptr, plugin_len, msg_ptr, msg_len) → void`**
//!   - No return value. Falls back to `info` level on parse failure.
//!
//! ## AI API (`trovato:kernel/ai-api`)
//!
//! - **`ai-request(req_ptr, req_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: request JSON read failed, `-3`: output write failed
//!   - `-20`: no provider configured for operation type
//!   - `-21`: HTTP request to provider failed
//!   - `-22`: rate limit exceeded (provider 429 or local RPM)
//!   - `-23`: malformed `AiRequest` JSON (or invalid message role)
//!   - `-24`: auth failure (401/403)
//!   - `-25`: provider error (non-2xx)
//!   - `-26`: token budget exceeded for the current period
//!   - `-27`: permission denied (user lacks `use ai` or operation-specific permission)
//!   - `≥ 0`: bytes written (JSON `AiResponse`)
//!
//! ## HTTP API (`trovato:kernel/http`)
//!
//! - **`request(req_ptr, req_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: request JSON read failed, `-3`: output write failed
//!   - `-30`: HTTP request failed (network/DNS/connection error)
//!   - `-31`: HTTP request timed out
//!   - `-32`: invalid URL (malformed, non-HTTP scheme, blocked)
//!   - `-33`: response body too large for output buffer
//!   - `≥ 0`: bytes written (JSON [`crate::types::HttpResponse`])
//!
//! ### Streaming fetch (`http-open` / `http-read` / `http-close`) — P11e / D-49, D-50
//!
//! Additive chunked-read companion to `request` for bodies too large to buffer
//! whole in WASM memory. `request` is unchanged. Shares the `-30`..`-33` codes on
//! open plus the streaming-specific codes below.
//!
//! - **`http-open(req_ptr, req_len, out_ptr, out_max_len) → i32`** (metadata
//!   p11j / G-HTTP-META: returns response status + headers alongside the handle)
//!   - `-1`: memory missing, `-2`: request JSON read failed, `-3`: output write failed
//!   - `-14`: malformed `HttpRequest` JSON (or invalid method)
//!   - `-30`/`-31`/`-32`: request failed / timed out / invalid-or-blocked URL (same
//!     SSRF fence as `request`)
//!   - `-33`: response metadata (an oversized header block) does not fit the output buffer
//!   - `-39`: `Content-Length` preflight exceeds the total-transfer ceiling
//!   - `-40`: too many concurrent open handles for this tap invocation
//!   - `≥ 0`: bytes written (JSON [`crate::types::HttpOpenResponse`]
//!     `{handle, status, headers}`; the Store-scoped handle travels in the JSON,
//!     valid only within this tap call)
//!
//! - **`http-read(handle, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-3`: output write failed
//!   - `-31`: per-read timeout or total-transfer wall-clock budget expired
//!   - `-37`: unknown / already-closed / cross-call handle
//!   - `-38`: network error reading the next chunk
//!   - `-39`: total-transfer ceiling exceeded mid-stream (catches `Content-Length` lies)
//!   - `0`: EOF (no more body); `> 0`: bytes written (≤ 64 KB, the tap I/O buffer)
//!
//! - **`http-close(handle) → i32`**
//!   - `-37`: unknown / already-closed handle
//!   - `0`: success (connection released, slot freed)
//!
//! ## Queue API (`trovato:kernel/queue`)
//!
//! `push` (unchanged, byte-identical ABI) returns `-1` memory missing, `-2`/`-3`
//! parameter read, `-4` malformed payload JSON, `-5` DB insert failed.
//!
//! - **`enqueue(queue_ptr, queue_len, payload_ptr, payload_len, opts_ptr, opts_len) → i32`**
//!   (P11d / D-48: additive; carries optional `priority`/`delay`)
//!   - `-1`: memory missing, `-2`: queue name read failed, `-3`: payload read failed,
//!     `-4`: opts read failed
//!   - `-34`: malformed payload JSON ([`ERR_QUEUE_INVALID_PAYLOAD`])
//!   - `-35`: malformed opts JSON ([`ERR_QUEUE_INVALID_OPTS`])
//!   - `-36`: DB insert failed ([`ERR_QUEUE_INSERT_FAILED`])
//!   - `0`: success
//!
//! ## Text format (`trovato:kernel/text-format`)
//!
//! - **`filter-text(text_ptr, text_len, format_ptr, format_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: text read failed, `-3`: format read failed
//!   - `-70`: the acting user may not use this format ([`ERR_TEXT_FORMAT_NOT_PERMITTED`])
//!   - `-71`: no such format ([`ERR_TEXT_FORMAT_UNKNOWN`])
//!   - `-72`: the filtered text does not fit the output buffer ([`ERR_TEXT_FORMAT_TOO_LARGE`])
//!   - `≥ 0`: bytes of filtered text written
//!
//! ## Query (`trovato:kernel/query`)
//!
//! - **`run(def_ptr, def_len, opt_ptr, opt_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: definition read failed, `-3`: options read failed,
//!     `-4`: output write failed
//!   - `-10`: no services, or no gather service, in this context
//!   - `-12`: the query failed, or ran past the 5 second statement timeout
//!   - `-13`: the result could not be serialized
//!   - `-14`: the definition or the options are not valid JSON of their shape
//!   - `-16`: a table the definition names is outside the plugin's allowlist, or
//!     protected ([`ERR_TABLE_NOT_DECLARED`])
//!   - `-60`: called while an item access decision is being made
//!     ([`ERR_ITEM_ACCESS_DENIED`])
//!   - `-61`: a content read from cron or the queue without `item_background`
//!     ([`ERR_ITEM_BACKGROUND_DENIED`])
//!   - `-80`: the definition is refused ([`ERR_QUERY_INVALID`])
//!   - `-81`: the definition exceeds a bound ([`ERR_QUERY_BOUND`])
//!   - `≥ 0`: bytes written (a JSON `GatherResult`)
//!
//! ## Vocabulary (`trovato:kernel/vocabulary`)
//!
//! - **`create-owned(req_ptr, req_len, out_ptr, out_max_len) → i32`** (B108)
//!   - `-1`: memory missing, `-2`: request read failed, `-3`: output write failed
//!   - `-10`: no services in this context
//!   - `-12`: the write failed
//!   - `-14`: the request is not valid JSON of its shape
//!   - `-90`: the owner type is not one this plugin declared ([`ERR_VOCABULARY_OWNER_UNDECLARED`])
//!   - `-91`: the id, label or owner id is not valid ([`ERR_VOCABULARY_INVALID`])
//!   - `-92`: a vocabulary with this id exists ([`ERR_VOCABULARY_EXISTS`])
//!   - `≥ 0`: bytes written (the vocabulary as JSON)
//!
//! - **`list-owned(type_ptr, type_len, id_ptr, id_len, out_ptr, out_max_len) → i32`** (B108)
//!   - `-1`, `-10`, `-12` as above; `-2`: owner type read failed, `-3`: owner id read or output write failed
//!   - `-90`: the owner type is not one this plugin declared
//!   - `≥ 0`: bytes written (a JSON array of vocabularies)
//!
//! ## SDK-side Errors (client-side, before/after WASM boundary)
//!
//! These errors are produced by the SDK wrapper functions in `host.rs`, not by host functions:
//!
//! - `-100` ([`ERR_SDK_SERIALIZE`]): JSON serialization failed before calling host
//! - `-101` ([`ERR_SDK_UTF8`]): UTF-8 decoding of host response buffer failed
//! - `-102` ([`ERR_SDK_DESERIALIZE`]): Host response JSON deserialization failed
//! - `-103` ([`ERR_SDK_OUTPUT_BUFFER_EXCEEDED`]): Result exceeded 256KB buffer (truncation prevented)

/// Memory export not found — the WASM module does not export `"memory"`.
pub const ERR_MEMORY_MISSING: i32 = -1;

/// First parameter read failed — UTF-8 decoding error or out-of-bounds slice.
pub const ERR_PARAM1_READ: i32 = -2;

/// Second parameter or output write failed — buffer too small or out of bounds.
pub const ERR_PARAM2_OR_OUTPUT: i32 = -3;

/// Third parameter read failed (used by DB functions with extra params like
/// `query-raw` and `insert`).
pub const ERR_PARAM3_READ: i32 = -4;

/// Services not available (tap executed without request context).
pub const ERR_NO_SERVICES: i32 = -10;

/// SQL statement rejected by DDL guard.
pub const ERR_DDL_REJECTED: i32 = -11;

/// SQL execution failed.
pub const ERR_SQL_FAILED: i32 = -12;

/// Result serialization failed.
pub const ERR_SERIALIZE_FAILED: i32 = -13;

/// JSON parameter deserialization failed.
pub const ERR_PARAM_DESERIALIZE: i32 = -14;

/// Invalid table or column name (must match `[a-zA-Z_][a-zA-Z0-9_]*`).
pub const ERR_INVALID_IDENTIFIER: i32 = -15;

/// Structured DB call (`select`/`insert`/`update`/`delete`) targeted a table
/// outside the plugin's effective allowlist — the tables its own migrations
/// create, unioned with its manifest `db_tables` (WASM-2 / D-19). The host logs
/// the declarative `table-not-declared: <table> (plugin <name>)` detail.
pub const ERR_TABLE_NOT_DECLARED: i32 = -16;

/// Raw-SQL host call (`query-raw`/`execute-raw`) made without the declared
/// `raw_sql` capability (WASM-2 / D-19). The host logs the declarative
/// `raw-sql-not-declared: <plugin>` detail.
pub const ERR_RAW_SQL_NOT_DECLARED: i32 = -17;

/// `insert-many` was given more than 500 rows. Refused whole, before anything
/// is written: split the batch.
///
/// Added by B80, after 0.105.0.
pub const ERR_DB_BATCH_TOO_LARGE: i32 = -82;

// =============================================================================
// AI API errors (`trovato:kernel/ai-api`)
// =============================================================================

/// No provider configured (or enabled) for the requested operation type.
pub const ERR_AI_NO_PROVIDER: i32 = -20;

/// HTTP request to the AI provider failed (timeout, network error, DNS).
pub const ERR_AI_REQUEST_FAILED: i32 = -21;

/// Rate limit exceeded — either the provider returned HTTP 429 or the
/// local per-provider RPM limit was reached.
pub const ERR_AI_RATE_LIMITED: i32 = -22;

/// Malformed `AiRequest` JSON from the plugin (deserialization failure).
pub const ERR_AI_INVALID_REQUEST: i32 = -23;

/// Provider returned 401 or 403 — API key is invalid or missing.
pub const ERR_AI_AUTH_FAILED: i32 = -24;

/// Provider returned a non-2xx error (500, 503, etc.).
pub const ERR_AI_PROVIDER_ERROR: i32 = -25;

/// Token budget exceeded for the current period.
pub const ERR_AI_BUDGET_EXCEEDED: i32 = -26;

/// Permission denied — the current user lacks `use ai` or the required
/// operation-specific AI permission.
pub const ERR_AI_PERMISSION_DENIED: i32 = -27;

/// Background AI denied — the request comes from a kernel background principal
/// (cron / queue worker, P11c / D-40) but the calling plugin did not declare the
/// `ai_background` manifest capability (D-41). Distinct from
/// [`ERR_AI_PERMISSION_DENIED`] (the human `use ai` permission plane), so the two
/// denials are separable in logs and tests.
pub const ERR_AI_BACKGROUND_DENIED: i32 = -28;

/// The requested [`crate::types::AiOperationType`] has no route on this
/// provider — either the kernel does not serve that operation yet
/// (`ImageGeneration`, `SpeechToText`, `TextToSpeech`, `Moderation`), or the
/// resolved provider's protocol exposes no endpoint for it (Anthropic has no
/// embeddings API).
///
/// Added in `KERNEL_API_VERSION (0,99)` (K1 fix 2, **G-AI-EMBED-UNROUTED**).
/// Before it, an unrouted operation was silently posted to `/chat/completions`
/// and the caller got a plausible-looking `AiResponse` back, discovering the
/// problem only when the "vector" failed to parse as a float array. A distinct
/// code is what makes that failure legible.
pub const ERR_AI_OPERATION_UNSUPPORTED: i32 = -41;

/// An `item-api` call was refused by the access decision: the requesting user
/// may not `view`, `edit` or `delete` that item, or may not create content of
/// that type.
///
/// `item-api` acts as the user the plugin is handling a request for, so this is
/// that user's own answer, not a statement about the plugin. A plugin that
/// needs an authority its caller does not have is a plugin that should not be
/// doing the work on a request path.
///
/// `get-item` does **not** return this: a denied read is indistinguishable from
/// a missing item there, and writes `null` exactly as a missing id does, so a
/// plugin cannot use the error to confirm that a draft exists.
pub const ERR_ITEM_ACCESS_DENIED: i32 = -60;

/// An `item-api` call was made from a **background** dispatch context — cron
/// (`tap_cron`) or the queue worker (`tap_queue_worker`) — by a plugin that did
/// not declare the `item_background` manifest capability.
///
/// A background context carries the kernel-internal background principal, which
/// has no identity and no permissions, so there is no user whose authority the
/// call could act with. Rather than fall back to kernel authority silently, the
/// kernel asks the plugin to say it needs it, on the same declared, auditable
/// manifest plane as [`ERR_AI_BACKGROUND_DENIED`]. With the capability, a
/// background call proceeds with kernel authority; without it, this.
pub const ERR_ITEM_BACKGROUND_DENIED: i32 = -61;

/// `delete-item` was given an id that names no item.
///
/// Deleting nothing used to return `0`, so a plugin could not tell a delete
/// that happened from one that did not (BL-25). An id the caller may not see is
/// not this: the delete is decided first, and a refusal is
/// [`ERR_ITEM_ACCESS_DENIED`].
///
/// Added by B68, after 0.104.0.
pub const ERR_ITEM_NOT_FOUND: i32 = -62;

/// An `item-api` write was made from inside item write taps nested
/// `MAX_ITEM_WRITE_DEPTH` (4) deep, and was refused before anything was written.
///
/// A plugin's `save-item` and `delete-item` fire the item write taps on the
/// other plugins, and a plugin handling one may write again. A nested write
/// never fires back into a plugin whose write tap is already running, and this
/// bounds the chain across plugins.
///
/// Added by B68, after 0.104.0.
pub const ERR_ITEM_WRITE_DEPTH: i32 = -63;

/// An `item-api` call was made on a dispatch path that carries no item service,
/// so nothing could be decided or written. The kernel logs which plugin and
/// call. Every server dispatch path carries one; this is a kernel wiring fault
/// or a test harness that never bound it, not something the plugin did.
///
/// Before U40 the same state answered [`ERR_NO_SERVICES`], which `tap_cron`
/// and `tap_queue_worker` returned for every write because their services
/// never had the item service bound.
pub const ERR_ITEM_SERVICE_UNAVAILABLE: i32 = -64;

// =============================================================================
// Text format errors (`trovato:kernel/text-format`)
// =============================================================================

/// The user the plugin is acting for may not use the requested text format:
/// `plain_text` is always allowed, `filtered_html` needs `use filtered_html`,
/// `full_html` needs `use full_html`. Nothing was filtered.
///
/// Added by B71, after 0.104.0.
pub const ERR_TEXT_FORMAT_NOT_PERMITTED: i32 = -70;

/// The requested text format does not exist. The kernel knows `plain_text`,
/// `filtered_html` and `full_html`.
///
/// Added by B71, after 0.104.0.
pub const ERR_TEXT_FORMAT_UNKNOWN: i32 = -71;

/// The filtered text is larger than the output buffer. Refused rather than
/// truncated, since cutting filtered HTML could leave a broken tag.
///
/// Added by B71, after 0.104.0.
pub const ERR_TEXT_FORMAT_TOO_LARGE: i32 = -72;

// =============================================================================
// Query errors (`trovato:kernel/query`)
// =============================================================================

/// The definition is refused: `stage_aware = false` on a content base, an
/// include, a `semantic_similarity` filter, a plugin-table base joining
/// content, a `url` filter that is not `equals` or `in`, or anything else the
/// gather validator rejects. The host log carries the reason.
///
/// Added by B80, after 0.105.0.
pub const ERR_QUERY_INVALID: i32 = -80;

/// The definition exceeds a bound: more than 100 rows per call, 32 filters
/// (grouped ones and `having` included), 8 sorts, 3 relationships, filter
/// groups nested past 3, or more than 8 `group_by` columns or aggregates. Refused rather than capped, so a plugin never
/// mistakes a truncated answer for the whole one.
///
/// Added by B80, after 0.105.0.
pub const ERR_QUERY_BOUND: i32 = -81;

// =============================================================================
// Vocabulary errors (`trovato:kernel/vocabulary`)
// =============================================================================

/// The owner type is not one the calling plugin declares under
/// `[[vocabulary_owners]]`: a plugin creates and lists vocabularies only for
/// the owners it declared.
///
/// Added by B108, after 0.106.0.
pub const ERR_VOCABULARY_OWNER_UNDECLARED: i32 = -90;

/// The vocabulary id is not a machine name (lowercase letters, digits and
/// underscores, at most 32), the label is empty, or the owner id is empty or
/// longer than 255.
///
/// Added by B108, after 0.106.0.
pub const ERR_VOCABULARY_INVALID: i32 = -91;

/// A vocabulary with this id exists, owned or not.
///
/// Added by B108, after 0.106.0.
pub const ERR_VOCABULARY_EXISTS: i32 = -92;

// =============================================================================
// HTTP API errors (`trovato:kernel/http`)
// =============================================================================

/// HTTP request failed (network error, DNS failure, connection refused).
pub const ERR_HTTP_REQUEST_FAILED: i32 = -30;

/// HTTP request timed out.
pub const ERR_HTTP_TIMEOUT: i32 = -31;

/// Invalid URL (malformed, non-HTTP scheme, or blocked destination).
pub const ERR_HTTP_INVALID_URL: i32 = -32;

/// Response body too large for the output buffer.
pub const ERR_HTTP_RESPONSE_TOO_LARGE: i32 = -33;

// -----------------------------------------------------------------------------
// HTTP streaming fetch (`http-open`/`http-read`/`http-close`) — P11e / D-49, D-50
// -----------------------------------------------------------------------------

/// Streaming `http-read`/`http-close`: the handle is unknown, already closed, or
/// belongs to a different tap invocation (handles are Store-scoped and cannot be
/// reused across calls).
pub const ERR_HTTP_HANDLE_INVALID: i32 = -37;

/// Streaming `http-read`: reading the next body chunk from the network failed
/// (connection reset, decode error).
pub const ERR_HTTP_STREAM_READ_FAILED: i32 = -38;

/// Streaming `http-open`/`http-read`: the fetch would exceed the plugin's
/// manifest-declared, kernel-capped total-transfer ceiling — detected either at
/// `Content-Length` preflight or as bytes accumulate mid-stream (which also
/// catches a server that under-reports `Content-Length`).
pub const ERR_HTTP_TRANSFER_BUDGET: i32 = -39;

/// Streaming `http-open`: this tap invocation already holds the maximum number of
/// concurrent open streaming handles.
pub const ERR_HTTP_TOO_MANY_HANDLES: i32 = -40;

// =============================================================================
// Queue API errors (`trovato:kernel/queue`) — P11d / D-48 additive `enqueue`
// =============================================================================

/// Queue `enqueue`: the payload was not well-formed JSON.
pub const ERR_QUEUE_INVALID_PAYLOAD: i32 = -34;

/// Queue `enqueue`: the opts argument was not a well-formed JSON object
/// (expected `{ "priority"?: int, "delay"?: int }`).
pub const ERR_QUEUE_INVALID_OPTS: i32 = -35;

/// Queue `enqueue`: the database insert failed.
pub const ERR_QUEUE_INSERT_FAILED: i32 = -36;

// =============================================================================
// Mail API errors (`trovato:kernel/mail`) — added at KERNEL_API_VERSION (0,101)
// =============================================================================

/// The site has no SMTP host configured, so nothing can be sent.
///
/// Not a defect in the call: a site that has not configured mail cannot send
/// any, and the kernel's own mail is skipped under the same condition. Report it
/// to the person rather than retrying.
pub const ERR_MAIL_NOT_CONFIGURED: i32 = -50;

/// The site has no contact address configured (`site_mail` is empty).
///
/// `send-to-site-contacts` sends to the site's own address and takes no
/// recipient from the caller, so with no address configured there is nowhere for
/// the message to go.
pub const ERR_MAIL_NO_RECIPIENT: i32 = -51;

/// The request was not a well-formed mail request.
///
/// Covers an unparseable payload, an empty subject or body, a subject carrying a
/// carriage return or newline (which would be header injection), an attachment
/// whose filename or declared content type is unusable, and attachment bytes
/// that are not valid base64.
pub const ERR_MAIL_INVALID_REQUEST: i32 = -52;

/// The attachments exceeded the per-message limits: too many of them, or too
/// many bytes in total. See the `mail` interface documentation for the ceilings.
pub const ERR_MAIL_ATTACHMENT_TOO_LARGE: i32 = -53;

/// The message was well-formed and delivery failed: the transport refused it, or
/// the shared SMTP circuit breaker is open. The kernel logs the reason.
pub const ERR_MAIL_SEND_FAILED: i32 = -54;

/// The plugin has sent as much mail as its bucket allows for now.
///
/// Per plugin and on every path, including cron and the queue worker. Nothing
/// was sent and the kernel logged the refusal. Distinct from
/// [`ERR_MAIL_SEND_FAILED`] because the answer is different: waiting fixes this
/// one, and retrying immediately only spends the next window.
pub const ERR_MAIL_RATE_LIMITED: i32 = -55;

/// The plugin called `send-to-user` without declaring `mail_users = true` under
/// `[capabilities]`. Declaring `mail` alone reaches the site contact address
/// only. Added by B73.
pub const ERR_MAIL_USERS_DENIED: i32 = -56;

/// The user cannot be mailed: no such user, a blocked account, or no address
/// on file. One code for all three, so a plugin learns nothing about the
/// account from the refusal. Nothing was sent. Added by B73.
pub const ERR_MAIL_RECIPIENT_UNAVAILABLE: i32 = -57;

/// The user opted out of this plugin's mail with the unsubscribe link. Nothing
/// was sent; do not retry. Added by B73.
pub const ERR_MAIL_OPTED_OUT: i32 = -58;

// =============================================================================
// User directory errors (`trovato:kernel/user-api` `load-users`,
// `find-user-by-name`, `find-users-by-name-prefix`) — added by B73, extended
// by B179
// =============================================================================

/// The plugin did not declare `user_directory = true` under `[capabilities]`.
///
/// Shared by `load-users`, `find-user-by-name` and `find-users-by-name-prefix`:
/// all three answer about users the plugin is not serving, so the grant is
/// separate from `user-api`, visible in the manifest and reviewable at install
/// time.
pub const ERR_USER_DIRECTORY_DENIED: i32 = -73;

/// An id was not a user id (a UUID), or the list was not a JSON array of
/// strings. Nothing was looked up.
pub const ERR_USER_IDS_INVALID: i32 = -74;

/// More than 100 ids in one call. Nothing was looked up; split the batch.
pub const ERR_USER_IDS_TOO_MANY: i32 = -75;

/// `find-user-by-name`'s name was empty or longer than 255 characters (the
/// width of `users.name`). Nothing was looked up. Added by B179.
pub const ERR_USER_NAME_INVALID: i32 = -76;

/// `find-users-by-name-prefix`'s prefix was empty or longer than 255
/// characters. Nothing was looked up. Added by B179.
pub const ERR_USER_NAME_PREFIX_INVALID: i32 = -77;

// =============================================================================
// SDK-side errors (client-side, before/after crossing WASM boundary)
// =============================================================================

/// JSON serialization failed on the SDK side (before crossing the WASM boundary).
pub const ERR_SDK_SERIALIZE: i32 = -100;

/// UTF-8 decoding failed when reading the host response buffer.
pub const ERR_SDK_UTF8: i32 = -101;

/// Failed to deserialize the host response JSON into the expected Rust type.
pub const ERR_SDK_DESERIALIZE: i32 = -102;

/// Result exceeded the maximum output buffer size (256KB).
///
/// The host function wrote data up to the buffer limit but the full result
/// was larger. The returned data would be truncated and invalid.
/// Plugins should reduce their result set (add LIMIT) or paginate.
pub const ERR_SDK_OUTPUT_BUFFER_EXCEEDED: i32 = -103;
