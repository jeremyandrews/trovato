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
use trovato_sdk::host::{query_raw, log};
use trovato_sdk::host_errors;

match query_raw("SELECT * FROM item WHERE type = $1 LIMIT 100", &[json!("conference")]) {
    Ok(result) => {
        // Process JSON result
    }
    Err(host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED) => {
        // Result too large — reduce LIMIT or paginate
        log("warn", "my_plugin", "query result exceeded 256KB buffer");
    }
    Err(host_errors::ERR_SQL_FAILED) => {
        // SQL error — log and return gracefully
        log("error", "my_plugin", "SQL query failed");
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
