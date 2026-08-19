//! Host function bindings for calling kernel services from WASM plugins.
//!
//! These functions are only usable when compiled for `wasm32` targets.
//! On native targets, stub implementations are provided for testing.

/// Maximum output buffer size for host function results (256KB).
///
/// If a host function fills the entire buffer, the SDK returns
/// [`crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED`] rather than
/// silently returning truncated data. Plugins should reduce result set
/// size (add SQL LIMIT) or paginate.
#[cfg(target_arch = "wasm32")]
const MAX_OUTPUT_BUFFER: usize = 256 * 1024;

/// Output buffer size for [`invoke`] results (1 MiB).
///
/// Matches the kernel's frozen 1 MiB payload/result cap (FR-4a), so any result
/// the host is willing to return fits without truncation.
#[cfg(target_arch = "wasm32")]
const INVOKE_MAX_BUFFER: usize = 1_048_576;

// --------------------------------------------------------------------------
// WASM extern declarations — available only when compiling for wasm32
// --------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/db")]
unsafe extern "C" {
    #[link_name = "execute-raw"]
    fn __db_execute_raw(sql_ptr: i32, sql_len: i32, params_ptr: i32, params_len: i32) -> i64;

    #[link_name = "query-raw"]
    fn __db_query_raw(
        sql_ptr: i32,
        sql_len: i32,
        params_ptr: i32,
        params_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/ai-api")]
unsafe extern "C" {
    #[link_name = "ai-request"]
    fn __ai_request(req_ptr: i32, req_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/http")]
unsafe extern "C" {
    #[link_name = "request"]
    fn __http_request(req_ptr: i32, req_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "http-open"]
    fn __http_open(req_ptr: i32, req_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "http-read"]
    fn __http_read(handle: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "http-close"]
    fn __http_close(handle: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/logging")]
unsafe extern "C" {
    #[link_name = "log"]
    fn __log(
        level_ptr: i32,
        level_len: i32,
        plugin_ptr: i32,
        plugin_len: i32,
        msg_ptr: i32,
        msg_len: i32,
    );
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/queue")]
unsafe extern "C" {
    #[link_name = "push"]
    fn __queue_push(
        queue_name_ptr: i32,
        queue_name_len: i32,
        payload_ptr: i32,
        payload_len: i32,
    ) -> i32;

    #[link_name = "enqueue"]
    fn __queue_enqueue(
        queue_name_ptr: i32,
        queue_name_len: i32,
        payload_ptr: i32,
        payload_len: i32,
        opts_ptr: i32,
        opts_len: i32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/mail")]
unsafe extern "C" {
    #[link_name = "send-to-site-contacts"]
    fn __mail_send_to_site_contacts(req_ptr: i32, req_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/crypto-api")]
unsafe extern "C" {
    #[link_name = "sha256"]
    fn __crypto_sha256(data_ptr: i32, data_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "hmac-sha256"]
    fn __crypto_hmac_sha256(
        key_ptr: i32,
        key_len: i32,
        msg_ptr: i32,
        msg_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;

    #[link_name = "random-bytes"]
    fn __crypto_random_bytes(len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "constant-time-eq"]
    fn __crypto_constant_time_eq(a_ptr: i32, a_len: i32, b_ptr: i32, b_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/plugin-api")]
unsafe extern "C" {
    #[link_name = "invoke"]
    fn __plugin_invoke(
        plugin_ptr: i32,
        plugin_len: i32,
        fn_ptr: i32,
        fn_len: i32,
        payload_ptr: i32,
        payload_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i64;

    #[link_name = "plugin-exists"]
    fn __plugin_exists(name_ptr: i32, name_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/user-api")]
unsafe extern "C" {
    #[link_name = "current-user-id"]
    fn __current_user_id(out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "current-user-has-permission"]
    fn __current_user_has_permission(perm_ptr: i32, perm_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/variables")]
unsafe extern "C" {
    #[link_name = "get"]
    fn __variables_get(
        name_ptr: i32,
        name_len: i32,
        default_ptr: i32,
        default_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;

    #[link_name = "set"]
    fn __variables_set(name_ptr: i32, name_len: i32, value_ptr: i32, value_len: i32) -> i32;
}

// --------------------------------------------------------------------------
// Ergonomic wrappers
// --------------------------------------------------------------------------

/// Execute a DML statement (INSERT, UPDATE, DELETE), return rows affected.
///
/// The kernel rejects DDL statements (CREATE, DROP, ALTER, TRUNCATE, GRANT, REVOKE).
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure.
#[cfg(target_arch = "wasm32")]
pub fn execute_raw(sql: &str, params: &[serde_json::Value]) -> Result<u64, i32> {
    let params_json =
        serde_json::to_string(params).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let result = unsafe {
        __db_execute_raw(
            sql.as_ptr() as i32,
            sql.len() as i32,
            params_json.as_ptr() as i32,
            params_json.len() as i32,
        )
    };
    if result < 0 {
        Err(result as i32)
    } else {
        Ok(result as u64)
    }
}

/// Execute a SELECT query, return JSON result string.
///
/// The kernel only allows SELECT and WITH statements.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure.
#[cfg(target_arch = "wasm32")]
pub fn query_raw(sql: &str, params: &[serde_json::Value]) -> Result<String, i32> {
    let params_json =
        serde_json::to_string(params).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __db_query_raw(
            sql.as_ptr() as i32,
            sql.len() as i32,
            params_json.as_ptr() as i32,
            params_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        let len = result as usize;
        if len >= MAX_OUTPUT_BUFFER {
            return Err(crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
        }
        buf.truncate(len);
        String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)
    }
}

/// Make an outbound HTTP request through the kernel.
///
/// The kernel executes the request on the plugin's behalf, enforcing
/// timeouts and security restrictions. Plugins cannot make direct
/// network calls from WASM.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure. See
/// [`crate::host_errors`] for HTTP-specific error codes (`ERR_HTTP_*`).
#[cfg(target_arch = "wasm32")]
pub fn http_request(
    request: &crate::types::HttpRequest,
) -> Result<crate::types::HttpResponse, i32> {
    let request_json =
        serde_json::to_string(request).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __http_request(
            request_json.as_ptr() as i32,
            request_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        let len = result as usize;
        if len >= MAX_OUTPUT_BUFFER {
            return Err(crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
        }
        buf.truncate(len);
        let json = String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)?;
        serde_json::from_str(&json).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
    }
}

/// Maximum bytes a single [`http_read`] draws from the kernel — the 64 KB tap
/// I/O buffer. The kernel never returns more than this per read regardless of the
/// buffer size passed, so a larger buffer buys nothing.
pub const HTTP_READ_CHUNK: usize = 64 * 1024;

/// Open a streaming HTTP fetch through the kernel (P11e / D-49; metadata p11j /
/// G-HTTP-META).
///
/// Additive companion to [`http_request`] for response bodies too large to buffer
/// whole in WASM memory (article HTML routinely exceeds 1 MB). Returns an
/// [`HttpOpenResponse`](crate::types::HttpOpenResponse) carrying the streaming
/// `handle` plus the response `status` and `headers` — the same status/header
/// representation as the one-shot [`http_request`], so conditional GET works on
/// the streaming path (a `304` is distinguishable from an empty `200`, and a
/// fresh `ETag`/`Last-Modified` is readable). Call [`http_read`] on the handle
/// repeatedly until it returns an empty slice (EOF), then [`http_close`] the
/// handle. A `304` response has no body, so the first [`http_read`] returns EOF
/// immediately. The kernel applies the same SSRF fence as [`http_request`] on
/// open and bounds the total bytes transferred by the plugin's
/// manifest-declared, kernel-capped total-transfer ceiling.
///
/// The handle is scoped to the current tap invocation and cannot be used from a
/// later or concurrent call.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure. See
/// [`crate::host_errors`] for the streaming-specific `ERR_HTTP_*` codes;
/// `ERR_HTTP_RESPONSE_TOO_LARGE` is returned if the response metadata (a
/// pathologically large header block) does not fit the output buffer.
#[cfg(target_arch = "wasm32")]
pub fn http_open(
    request: &crate::types::HttpRequest,
) -> Result<crate::types::HttpOpenResponse, i32> {
    let request_json =
        serde_json::to_string(request).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __http_open(
            request_json.as_ptr() as i32,
            request_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        let len = result as usize;
        if len >= MAX_OUTPUT_BUFFER {
            return Err(crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
        }
        buf.truncate(len);
        let json = String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)?;
        serde_json::from_str(&json).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
    }
}

/// Read up to 64 KB of the next body bytes from an open streaming handle
/// (P11e / D-49).
///
/// Returns the bytes read; an **empty** vector signals EOF. The kernel caps each
/// read at [`HTTP_READ_CHUNK`] (the tap I/O buffer) regardless of demand, so the
/// caller loops until it gets an empty slice.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure — an unknown or closed
/// handle, a read timeout, a network error, or the total-transfer ceiling being
/// exceeded. See [`crate::host_errors`].
#[cfg(target_arch = "wasm32")]
pub fn http_read(handle: u32) -> Result<Vec<u8>, i32> {
    let mut buf = vec![0u8; HTTP_READ_CHUNK];
    let n = unsafe { __http_read(handle as i32, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n < 0 {
        Err(n)
    } else {
        buf.truncate(n as usize);
        Ok(buf)
    }
}

/// Close a streaming handle, releasing its connection (P11e / D-49).
///
/// # Errors
///
/// Returns [`crate::host_errors::ERR_HTTP_HANDLE_INVALID`] if the handle is
/// unknown or already closed.
#[cfg(target_arch = "wasm32")]
pub fn http_close(handle: u32) -> Result<(), i32> {
    let result = unsafe { __http_close(handle as i32) };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Push a job onto a named plugin queue.
///
/// The kernel associates the job with the calling plugin automatically.
/// The cron task will drain the queue and call `tap_queue_worker` with
/// each job's payload.
///
/// # Errors
///
/// Returns a negative error code if the kernel rejects the push (bad JSON,
/// DB error, etc.).
#[cfg(target_arch = "wasm32")]
pub fn queue_push(queue_name: &str, payload: &serde_json::Value) -> Result<(), i32> {
    let payload_json =
        serde_json::to_string(payload).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let result = unsafe {
        __queue_push(
            queue_name.as_ptr() as i32,
            queue_name.len() as i32,
            payload_json.as_ptr() as i32,
            payload_json.len() as i32,
        )
    };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Push a job onto a named plugin queue (stub for native testing, always succeeds).
#[cfg(not(target_arch = "wasm32"))]
pub fn queue_push(_queue_name: &str, _payload: &serde_json::Value) -> Result<(), i32> {
    Ok(())
}

/// Push a job onto a named plugin queue with options (P11d / D-48).
///
/// Additive companion to [`queue_push`]: carries an optional priority (higher
/// values drain first) and an optional delay (seconds to defer the first
/// attempt) via [`crate::types::QueueOptions`]. The kernel associates the job
/// with the calling plugin automatically and applies v2 retry/backoff/
/// dead-letter semantics regardless of entry point.
///
/// # Errors
///
/// Returns a negative host error code (see [`crate::host_errors`]) if
/// serialization fails or the kernel rejects the enqueue (bad JSON, DB error).
#[cfg(target_arch = "wasm32")]
pub fn queue_enqueue(
    queue_name: &str,
    payload: &serde_json::Value,
    opts: &crate::types::QueueOptions,
) -> Result<(), i32> {
    let payload_json =
        serde_json::to_string(payload).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let opts_json =
        serde_json::to_string(opts).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let result = unsafe {
        __queue_enqueue(
            queue_name.as_ptr() as i32,
            queue_name.len() as i32,
            payload_json.as_ptr() as i32,
            payload_json.len() as i32,
            opts_json.as_ptr() as i32,
            opts_json.len() as i32,
        )
    };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Push a job with options (stub for native testing, always succeeds).
#[cfg(not(target_arch = "wasm32"))]
pub fn queue_enqueue(
    _queue_name: &str,
    _payload: &serde_json::Value,
    _opts: &crate::types::QueueOptions,
) -> Result<(), i32> {
    Ok(())
}

/// The mail request as it crosses the boundary: JSON, with attachment bytes
/// base64-encoded because JSON has no byte string.
#[derive(serde::Serialize)]
struct MailRequestWire<'a> {
    subject: &'a str,
    body: &'a str,
    attachments: Vec<MailAttachmentWire<'a>>,
}

/// One attachment on the wire.
#[derive(serde::Serialize)]
struct MailAttachmentWire<'a> {
    filename: &'a str,
    content_type: &'a str,
    bytes_base64: String,
}

/// Build the wire form of a mail request. Separated from the send so it can be
/// tested off-wasm, where the host function does not exist.
fn mail_request_json(
    subject: &str,
    body: &str,
    attachments: &[crate::types::MailAttachment],
) -> Result<String, i32> {
    let wire = MailRequestWire {
        subject,
        body,
        attachments: attachments
            .iter()
            .map(|a| MailAttachmentWire {
                filename: &a.filename,
                content_type: &a.content_type,
                bytes_base64: base64_encode(&a.bytes),
            })
            .collect(),
    };
    serde_json::to_string(&wire).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)
}

/// Send a message to the site's configured contact address.
///
/// **The recipient is not a parameter, deliberately.** The kernel sends to the
/// site's own `site_mail` address and nowhere else, so this cannot be used to
/// reach an arbitrary address. It covers the case a CMS needs a plugin to cover,
/// a visitor reaching the site owner, and it is useless as a relay.
///
/// Delivery uses the site's own SMTP transport, `from` address and circuit
/// breaker. A plugin cannot configure its own.
///
/// Requires `"mail"` in the plugin's `[capabilities] host_interfaces`.
///
/// # Errors
///
/// Returns a negative host error code (see [`crate::host_errors`]): the
/// `ERR_MAIL_*` family covers an unconfigured SMTP host, an unconfigured site
/// contact address, a malformed request (an empty subject or body, a control
/// character in the subject, an unusable attachment) and a delivery failure.
#[cfg(target_arch = "wasm32")]
pub fn mail_send_to_site_contacts(
    subject: &str,
    body: &str,
    attachments: &[crate::types::MailAttachment],
) -> Result<(), i32> {
    let request_json = mail_request_json(subject, body, attachments)?;
    let result = unsafe {
        __mail_send_to_site_contacts(request_json.as_ptr() as i32, request_json.len() as i32)
    };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Send to the site contact address (stub for native testing, always succeeds).
///
/// Still builds the request, so a plugin's own tests exercise the encoding path
/// rather than skipping it.
#[cfg(not(target_arch = "wasm32"))]
pub fn mail_send_to_site_contacts(
    subject: &str,
    body: &str,
    attachments: &[crate::types::MailAttachment],
) -> Result<(), i32> {
    mail_request_json(subject, body, attachments).map(|_| ())
}

/// Base64-encode bytes with the standard alphabet and padding (RFC 4648 §4).
///
/// Hand-written rather than pulled in as a dependency: the SDK is compiled into
/// every plugin's wasm and carries four dependencies on purpose, and this is the
/// only place any plugin needs base64. The encoder is 20 lines and pinned to the
/// RFC's own test vectors below.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18 & 0x3F) as usize] as char);
        out.push(ALPHABET[(triple >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Compute the hex-encoded SHA-256 hash of `data` through the kernel.
///
/// Plugins use the kernel's crypto host functions instead of bundling
/// their own crypto libraries into WASM.
///
/// # Errors
///
/// Returns a negative host error code on failure.
#[cfg(target_arch = "wasm32")]
pub fn crypto_sha256(data: &str) -> Result<String, i32> {
    // SHA-256 hex is always 64 chars.
    let mut buf = vec![0u8; 64];
    let result = unsafe {
        __crypto_sha256(
            data.as_ptr() as i32,
            data.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        buf.truncate(result as usize);
        String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)
    }
}

/// Compute the hex-encoded HMAC-SHA256 of `message` keyed by `key`.
///
/// # Errors
///
/// Returns a negative host error code on failure.
#[cfg(target_arch = "wasm32")]
pub fn crypto_hmac_sha256(key: &str, message: &str) -> Result<String, i32> {
    // HMAC-SHA256 hex is always 64 chars.
    let mut buf = vec![0u8; 64];
    let result = unsafe {
        __crypto_hmac_sha256(
            key.as_ptr() as i32,
            key.len() as i32,
            message.as_ptr() as i32,
            message.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        buf.truncate(result as usize);
        String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)
    }
}

/// Get `len` cryptographically-secure random bytes, hex-encoded.
///
/// The kernel rejects `len` outside the range `1..=256`.
///
/// # Errors
///
/// Returns a negative host error code on failure (including out-of-range `len`).
#[cfg(target_arch = "wasm32")]
pub fn crypto_random_bytes(len: u32) -> Result<String, i32> {
    // Up to 256 bytes -> 512 hex chars.
    let mut buf = vec![0u8; 512];
    let result =
        unsafe { __crypto_random_bytes(len as i32, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if result < 0 {
        Err(result)
    } else {
        buf.truncate(result as usize);
        String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)
    }
}

/// Compare two byte strings in constant time (timing-attack safe).
///
/// Returns `true` if equal, `false` otherwise (including on host failure).
#[cfg(target_arch = "wasm32")]
pub fn crypto_constant_time_eq(a: &str, b: &str) -> bool {
    let result = unsafe {
        __crypto_constant_time_eq(
            a.as_ptr() as i32,
            a.len() as i32,
            b.as_ptr() as i32,
            b.len() as i32,
        )
    };
    result == 1
}

/// Compute a SHA-256 hash (stub for native testing, returns empty string).
#[cfg(not(target_arch = "wasm32"))]
pub fn crypto_sha256(_data: &str) -> Result<String, i32> {
    Ok(String::new())
}

/// Compute an HMAC-SHA256 (stub for native testing, returns empty string).
#[cfg(not(target_arch = "wasm32"))]
pub fn crypto_hmac_sha256(_key: &str, _message: &str) -> Result<String, i32> {
    Ok(String::new())
}

/// Get random bytes (stub for native testing, returns empty string).
#[cfg(not(target_arch = "wasm32"))]
pub fn crypto_random_bytes(_len: u32) -> Result<String, i32> {
    Ok(String::new())
}

/// Constant-time compare (stub for native testing; plain, non-constant-time equality).
#[cfg(not(target_arch = "wasm32"))]
pub fn crypto_constant_time_eq(a: &str, b: &str) -> bool {
    a == b
}

/// Get the current user's ID as a string.
///
/// Returns an empty string if no user is authenticated or the host
/// function fails.
#[cfg(target_arch = "wasm32")]
pub fn current_user_id() -> String {
    let mut buf = vec![0u8; 256];
    let result = unsafe { __current_user_id(buf.as_mut_ptr() as i32, buf.len() as i32) };
    if result <= 0 {
        return String::new();
    }
    buf.truncate(result as usize);
    String::from_utf8(buf).unwrap_or_default()
}

/// Get the current user's ID (stub for native testing, returns empty).
#[cfg(not(target_arch = "wasm32"))]
pub fn current_user_id() -> String {
    String::new()
}

/// Check if the current user has a specific permission.
///
/// Returns `true` if the user has the permission, `false` otherwise
/// (including on host function failure).
#[cfg(target_arch = "wasm32")]
pub fn current_user_has_permission(permission: &str) -> bool {
    let result = unsafe {
        __current_user_has_permission(permission.as_ptr() as i32, permission.len() as i32)
    };
    result == 1
}

/// Check permission (stub for native testing, always returns true).
#[cfg(not(target_arch = "wasm32"))]
pub fn current_user_has_permission(_permission: &str) -> bool {
    true
}

/// Get a site variable by name, with a default fallback.
///
/// Variables are persistent key-value configuration stored in the
/// database. Returns the default value if the variable is not set.
///
/// # Errors
///
/// Returns a negative error code on host function failure.
#[cfg(target_arch = "wasm32")]
pub fn variables_get(name: &str, default: &str) -> Result<String, i32> {
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __variables_get(
            name.as_ptr() as i32,
            name.len() as i32,
            default.as_ptr() as i32,
            default.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        let len = result as usize;
        if len >= MAX_OUTPUT_BUFFER {
            return Err(crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
        }
        buf.truncate(len);
        String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)
    }
}

/// Get a site variable (stub for native testing, returns default).
#[cfg(not(target_arch = "wasm32"))]
pub fn variables_get(_name: &str, default: &str) -> Result<String, i32> {
    Ok(default.to_string())
}

/// Set a site variable.
///
/// # Errors
///
/// Returns a negative error code on failure, 0 on success.
#[cfg(target_arch = "wasm32")]
pub fn variables_set(name: &str, value: &str) -> Result<(), i32> {
    let result = unsafe {
        __variables_set(
            name.as_ptr() as i32,
            name.len() as i32,
            value.as_ptr() as i32,
            value.len() as i32,
        )
    };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Set a site variable (stub for native testing, always succeeds).
#[cfg(not(target_arch = "wasm32"))]
pub fn variables_set(_name: &str, _value: &str) -> Result<(), i32> {
    Ok(())
}

/// Log a message through the kernel's tracing system.
///
/// Valid levels: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`.
#[cfg(target_arch = "wasm32")]
pub fn log(level: &str, plugin_name: &str, message: &str) {
    unsafe {
        __log(
            level.as_ptr() as i32,
            level.len() as i32,
            plugin_name.as_ptr() as i32,
            plugin_name.len() as i32,
            message.as_ptr() as i32,
            message.len() as i32,
        );
    }
}

/// Log a message (stub for native testing, prints to stderr).
#[cfg(not(target_arch = "wasm32"))]
pub fn log(level: &str, plugin_name: &str, message: &str) {
    eprintln!("[{level}] {plugin_name}: {message}");
}

/// Make an AI request through the kernel's provider registry.
///
/// The kernel resolves the provider, injects the API key, makes the HTTP
/// request, and returns a normalized response. API keys never cross the
/// WASM boundary.
///
/// # Errors
///
/// Returns the host error code (negative i32) on failure. See
/// [`crate::host_errors`] for AI-specific error codes.
#[cfg(target_arch = "wasm32")]
pub fn ai_request(request: &crate::types::AiRequest) -> Result<crate::types::AiResponse, i32> {
    let request_json =
        serde_json::to_string(request).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __ai_request(
            request_json.as_ptr() as i32,
            request_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        let len = result as usize;
        if len >= MAX_OUTPUT_BUFFER {
            return Err(crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
        }
        buf.truncate(len);
        let json = String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)?;
        serde_json::from_str(&json).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
    }
}

/// Invoke a published function on another plugin (FR-4a plugin-to-plugin call).
///
/// The kernel routes the call to `plugin`'s `function` export, passing `payload`
/// (an opaque UTF-8 JSON string the caller and callee agree on) and returning the
/// target function's JSON result.
///
/// To call another plugin at all, this plugin must declare
/// `host_interfaces = ["plugin-api"]`; the target must list `function` in its own
/// `[capabilities].public_functions` (deny-by-default).
///
/// # Errors
///
/// Returns an error string beginning with a frozen kebab prefix:
/// `target-not-found`, `permission-denied`, `function-not-exported`,
/// `target-errored`, or `payload-too-large` (payload/result over 1 MiB). Branch
/// on the prefix; the suffix is informational human detail.
#[cfg(target_arch = "wasm32")]
pub fn invoke(plugin: &str, function: &str, payload: &str) -> Result<String, String> {
    let mut buf = vec![0u8; INVOKE_MAX_BUFFER];
    let r = unsafe {
        __plugin_invoke(
            plugin.as_ptr() as i32,
            plugin.len() as i32,
            function.as_ptr() as i32,
            function.len() as i32,
            payload.as_ptr() as i32,
            payload.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    // ABI: r >= 0 ⇒ Ok, out[0..r]; r < 0 ⇒ Err, out[0..(-r - 1)] (see kernel
    // host/plugin_api.rs module ABI note).
    if r >= 0 {
        buf.truncate(r as usize);
        String::from_utf8(buf).map_err(|_| "target-errored: invalid UTF-8 result".to_string())
    } else {
        let n = ((-r) - 1) as usize;
        buf.truncate(n.min(INVOKE_MAX_BUFFER));
        Err(String::from_utf8(buf)
            .unwrap_or_else(|_| "target-errored: invalid UTF-8 error".to_string()))
    }
}

/// Check whether another plugin is installed, enabled, and exposes ≥1 publicly
/// invocable function (i.e. whether [`invoke`] could reach it).
///
/// This is invocability-aware, not a plain installed-check: a plugin that is
/// installed but publishes no functions returns `false`.
#[cfg(target_arch = "wasm32")]
pub fn plugin_exists(plugin: &str) -> bool {
    let r = unsafe { __plugin_exists(plugin.as_ptr() as i32, plugin.len() as i32) };
    r == 1
}

// --------------------------------------------------------------------------
// Native stubs for testing — no actual DB access
// --------------------------------------------------------------------------

/// Invoke a function on another plugin (stub for native testing).
///
/// Native builds have no kernel host, so this always reports the target as
/// unresolvable.
#[cfg(not(target_arch = "wasm32"))]
pub fn invoke(_plugin: &str, _function: &str, _payload: &str) -> Result<String, String> {
    Err("target-not-found: invoke unavailable in native test stub".to_string())
}

/// Check whether another plugin is invocable (stub for native testing, always false).
#[cfg(not(target_arch = "wasm32"))]
pub fn plugin_exists(_plugin: &str) -> bool {
    false
}

/// Make an outbound HTTP request (stub for native testing, returns mock 200).
#[cfg(not(target_arch = "wasm32"))]
pub fn http_request(
    _request: &crate::types::HttpRequest,
) -> Result<crate::types::HttpResponse, i32> {
    Ok(crate::types::HttpResponse {
        status: 200,
        headers: std::collections::HashMap::new(),
        body: "[]".to_string(),
    })
}

/// Open a streaming HTTP fetch (stub for native testing, returns handle 0 with a
/// mock 200 and no headers).
#[cfg(not(target_arch = "wasm32"))]
pub fn http_open(
    _request: &crate::types::HttpRequest,
) -> Result<crate::types::HttpOpenResponse, i32> {
    Ok(crate::types::HttpOpenResponse::new(
        0,
        200,
        std::collections::HashMap::new(),
    ))
}

/// Read from a streaming handle (stub for native testing, returns EOF).
#[cfg(not(target_arch = "wasm32"))]
pub fn http_read(_handle: u32) -> Result<Vec<u8>, i32> {
    Ok(Vec::new())
}

/// Close a streaming handle (stub for native testing, always succeeds).
#[cfg(not(target_arch = "wasm32"))]
pub fn http_close(_handle: u32) -> Result<(), i32> {
    Ok(())
}

/// Execute a DML statement (stub for native testing, always returns 0).
#[cfg(not(target_arch = "wasm32"))]
pub fn execute_raw(_sql: &str, _params: &[serde_json::Value]) -> Result<u64, i32> {
    Ok(0)
}

/// Execute a SELECT query (stub for native testing, always returns empty array).
#[cfg(not(target_arch = "wasm32"))]
pub fn query_raw(_sql: &str, _params: &[serde_json::Value]) -> Result<String, i32> {
    Ok("[]".to_string())
}

/// Make an AI request (stub for native testing, returns a mock response).
#[cfg(not(target_arch = "wasm32"))]
pub fn ai_request(_request: &crate::types::AiRequest) -> Result<crate::types::AiResponse, i32> {
    Ok(crate::types::AiResponse::new(
        "Mock AI response".to_string(),
        "test-model".to_string(),
        crate::types::AiUsage::default(),
        0,
        Some("stop".to_string()),
    ))
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn http_request_stub_returns_mock() {
        let request = crate::types::HttpRequest::get("https://example.com");
        let response = http_request(&request).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "[]");
    }

    #[test]
    fn execute_raw_stub_returns_zero() {
        let result = execute_raw("UPDATE item SET status = 1", &[]);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn query_raw_stub_returns_empty() {
        let result = query_raw("SELECT 1", &[]);
        assert_eq!(result.unwrap(), "[]");
    }

    #[test]
    fn execute_raw_with_params() {
        let params = vec![serde_json::json!(42), serde_json::json!("hello")];
        let result = execute_raw("UPDATE foo SET bar = $1 WHERE name = $2", &params);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn current_user_id_stub_returns_empty() {
        assert!(current_user_id().is_empty());
    }

    #[test]
    fn current_user_has_permission_stub_returns_true() {
        assert!(current_user_has_permission("use ai"));
    }

    #[test]
    fn variables_get_stub_returns_default() {
        let result = variables_get("some.key", "fallback").unwrap();
        assert_eq!(result, "fallback");
    }

    #[test]
    fn variables_set_stub_succeeds() {
        assert!(variables_set("some.key", "value").is_ok());
    }

    #[test]
    fn queue_push_stub_succeeds() {
        let result = queue_push("emails", &serde_json::json!({"to": "x@example.com"}));
        assert!(result.is_ok());
    }

    #[test]
    fn crypto_sha256_stub_is_callable() {
        assert!(crypto_sha256("abc").is_ok());
    }

    #[test]
    fn crypto_hmac_sha256_stub_is_callable() {
        assert!(crypto_hmac_sha256("key", "message").is_ok());
    }

    #[test]
    fn crypto_random_bytes_stub_is_callable() {
        assert!(crypto_random_bytes(16).is_ok());
    }

    #[test]
    fn crypto_constant_time_eq_stub_compares() {
        assert!(crypto_constant_time_eq("secret", "secret"));
        assert!(!crypto_constant_time_eq("secret", "guess"));
    }

    #[test]
    fn invoke_stub_reports_target_not_found() {
        let result = invoke("other_plugin", "some_fn", "{}");
        assert!(result.is_err());
        assert!(result.unwrap_err().starts_with("target-not-found"));
    }

    #[test]
    fn plugin_exists_stub_returns_false() {
        assert!(!plugin_exists("other_plugin"));
    }

    /// RFC 4648 §10's own vectors, which is the point of hand-writing the encoder
    /// rather than trusting it.
    #[test]
    fn base64_encode_matches_the_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_covers_the_whole_alphabet_and_high_bytes() {
        // 0x00..=0xFF exercises every 6-bit group, including the `+` and `/`
        // characters an incomplete alphabet would get wrong.
        let all: Vec<u8> = (0u8..=255).collect();
        let encoded = base64_encode(&all);
        assert_eq!(encoded.len(), 344);
        assert!(encoded.contains('+'), "{encoded}");
        assert!(encoded.contains('/'), "{encoded}");
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
        // Padding: 256 is not a multiple of 3, so the last group is short.
        assert!(encoded.ends_with("=="), "{encoded}");
    }

    #[test]
    fn a_mail_request_carries_its_attachments_base64_encoded() {
        let attachments = vec![crate::types::MailAttachment::text("notes.txt", "hi")];

        let json = mail_request_json("Subject", "Body", &attachments).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["subject"], "Subject");
        assert_eq!(parsed["body"], "Body");
        assert_eq!(parsed["attachments"][0]["filename"], "notes.txt");
        assert_eq!(parsed["attachments"][0]["content_type"], "text/plain");
        assert_eq!(parsed["attachments"][0]["bytes_base64"], "aGk=");
    }

    #[test]
    fn a_mail_request_with_no_attachments_still_carries_the_field() {
        let json = mail_request_json("Subject", "Body", &[]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["attachments"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn the_native_mail_stub_builds_the_request_rather_than_skipping_it() {
        assert!(mail_send_to_site_contacts("Subject", "Body", &[]).is_ok());
    }

    #[test]
    fn ai_request_stub_returns_mock() {
        use crate::types::{AiMessage, AiOperationType, AiRequest, AiRequestOptions};

        let request = AiRequest {
            operation: AiOperationType::Chat,
            provider_id: None,
            model: None,
            messages: vec![AiMessage::user("Hello")],
            input: None,
            options: AiRequestOptions::default(),
        };

        let response = ai_request(&request).unwrap();
        assert_eq!(response.content, "Mock AI response");
        assert_eq!(response.model, "test-model");
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    }
}
