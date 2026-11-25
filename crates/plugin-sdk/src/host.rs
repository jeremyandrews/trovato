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

/// Output buffer size for [`invoke`] and [`query`] results (1 MiB).
///
/// Matches the kernel's frozen 1 MiB payload/result cap (FR-4a), so any result
/// the host is willing to return fits without truncation; a page of 100 item
/// rows can pass the 256 KiB every other call gets.
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

    #[link_name = "send-to-user"]
    fn __mail_send_to_user(req_ptr: i32, req_len: i32) -> i32;
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

    #[link_name = "load-users"]
    fn __load_users(ids_ptr: i32, ids_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "find-user-by-name"]
    fn __find_user_by_name(name_ptr: i32, name_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "find-users-by-name-prefix"]
    fn __find_users_by_name_prefix(
        prefix_ptr: i32,
        prefix_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/text-format")]
unsafe extern "C" {
    #[link_name = "filter-text"]
    fn __filter_text(
        text_ptr: i32,
        text_len: i32,
        format_ptr: i32,
        format_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/vocabulary")]
unsafe extern "C" {
    #[link_name = "create-owned"]
    fn __create_owned_vocabulary(req_ptr: i32, req_len: i32, out_ptr: i32, out_max_len: i32)
    -> i32;
    #[link_name = "list-owned"]
    fn __list_owned_vocabularies(
        type_ptr: i32,
        type_len: i32,
        id_ptr: i32,
        id_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/query")]
unsafe extern "C" {
    #[link_name = "run"]
    fn __query_run(
        def_ptr: i32,
        def_len: i32,
        opt_ptr: i32,
        opt_len: i32,
        out_ptr: i32,
        out_max_len: i32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/item-api")]
unsafe extern "C" {
    #[link_name = "get-item"]
    fn __item_get(id_ptr: i32, id_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "save-item"]
    fn __item_save(item_ptr: i32, item_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "delete-item"]
    fn __item_delete(id_ptr: i32, id_len: i32) -> i32;

    #[link_name = "query-items"]
    fn __item_query(query_ptr: i32, query_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
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

/// Call an `item-api` function that writes JSON into an output buffer, and
/// parse what it wrote.
#[cfg(target_arch = "wasm32")]
fn item_call<T: serde::de::DeserializeOwned>(call: impl FnOnce(i32, i32) -> i32) -> Result<T, i32> {
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = call(buf.as_mut_ptr() as i32, buf.len() as i32);
    if result < 0 {
        return Err(result);
    }
    let len = result as usize;
    if len >= MAX_OUTPUT_BUFFER {
        return Err(crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
    }
    buf.truncate(len);
    serde_json::from_slice(&buf).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
}

/// Run a Gather definition for the request's viewer, stages and language.
///
/// `definition` is a Gather `QueryDefinition`, the JSON a gather is stored as;
/// `options` is `{"page", "per_page", "url_args"}`, every key optional
/// (`per_page` defaults to 20 and is at most 100). A definition over `item`
/// reads only what the viewer may see, on the request's stages, in its
/// language, and each row carries `url`; a filter on `url` looks an item up by
/// the address it is served at. A definition over one of the plugin's own
/// tables reads it as it is. Declare `query` in `host_interfaces`; from cron or a
/// queue worker a content read needs `item_background`. See
/// `docs/design/plugin-query-surface.md`.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Query":
/// [`ERR_TABLE_NOT_DECLARED`](crate::host_errors::ERR_TABLE_NOT_DECLARED),
/// [`ERR_QUERY_INVALID`](crate::host_errors::ERR_QUERY_INVALID),
/// [`ERR_QUERY_BOUND`](crate::host_errors::ERR_QUERY_BOUND),
/// [`ERR_ITEM_BACKGROUND_DENIED`](crate::host_errors::ERR_ITEM_BACKGROUND_DENIED).
#[cfg(target_arch = "wasm32")]
pub fn query(
    definition: &serde_json::Value,
    options: &serde_json::Value,
) -> Result<crate::types::QueryResult, i32> {
    let def =
        serde_json::to_string(definition).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let opt = serde_json::to_string(options).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; INVOKE_MAX_BUFFER];
    let result = unsafe {
        __query_run(
            def.as_ptr() as i32,
            def.len() as i32,
            opt.as_ptr() as i32,
            opt.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    let len = result as usize;
    if len >= INVOKE_MAX_BUFFER {
        return Err(crate::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
    }
    buf.truncate(len);
    serde_json::from_slice(&buf).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
}

/// Load an item by id, as the user the plugin is acting for.
///
/// `Ok(None)` when the item does not exist **or** the user may not view it; the
/// two are one answer on purpose. Fields the user may not view are dropped.
/// From cron or a queue worker the plugin needs `item_background`.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Item API".
#[cfg(target_arch = "wasm32")]
pub fn get_item(id: &str) -> Result<Option<crate::types::Item>, i32> {
    item_call(|out, max| unsafe { __item_get(id.as_ptr() as i32, id.len() as i32, out, max) })
}

/// Create or update an item through the kernel's item service, exactly as a
/// person's edit would: the item write taps fire on the other plugins, the
/// field and publish gates apply, and the item cache is refreshed.
///
/// `item` is a JSON object. With a non-nil `"id"` it is an update of `title`,
/// `status`, `fields` and `log`, whichever are present; `Ok(None)` means no
/// item has that id. Without one it is a create, which needs `"type"` and
/// takes `title`, `status`, `fields`, `log` and `language`.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Item API". A refusal is
/// [`crate::host_errors::ERR_ITEM_ACCESS_DENIED`].
#[cfg(target_arch = "wasm32")]
pub fn save_item(item: &serde_json::Value) -> Result<Option<crate::types::Item>, i32> {
    let json = serde_json::to_string(item).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    item_call(|out, max| unsafe { __item_save(json.as_ptr() as i32, json.len() as i32, out, max) })
}

/// Delete an item through the kernel's item service; `tap_item_delete` fires on
/// the other plugins.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Item API". An id that names
/// no item is [`crate::host_errors::ERR_ITEM_NOT_FOUND`].
#[cfg(target_arch = "wasm32")]
pub fn delete_item(id: &str) -> Result<(), i32> {
    let result = unsafe { __item_delete(id.as_ptr() as i32, id.len() as i32) };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// List items the user the plugin is acting for may view.
///
/// `query` is a JSON object with optional `type`, `status`, `limit` (at most
/// 100, default 50) and `offset`. Only the request's read stages are listed
/// (Live outside a plugin route), newest change first, ties broken by id. A
/// page can come back shorter than `limit`: items the user may not view are
/// left out, not counted. For a listing, [`query`] sorts, joins, pages and
/// returns each row's URL.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Item API".
#[cfg(target_arch = "wasm32")]
pub fn query_items(query: &serde_json::Value) -> Result<Vec<crate::types::Item>, i32> {
    let json = serde_json::to_string(query).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    item_call(|out, max| unsafe { __item_query(json.as_ptr() as i32, json.len() as i32, out, max) })
}

/// Run a Gather definition (stub for native testing, always an empty page).
#[cfg(not(target_arch = "wasm32"))]
pub fn query(
    _definition: &serde_json::Value,
    _options: &serde_json::Value,
) -> Result<crate::types::QueryResult, i32> {
    Ok(crate::types::QueryResult::default())
}

/// Load an item (stub for native testing, always `Ok(None)`).
#[cfg(not(target_arch = "wasm32"))]
pub fn get_item(_id: &str) -> Result<Option<crate::types::Item>, i32> {
    Ok(None)
}

/// Save an item (stub for native testing, always `Ok(None)`).
#[cfg(not(target_arch = "wasm32"))]
pub fn save_item(_item: &serde_json::Value) -> Result<Option<crate::types::Item>, i32> {
    Ok(None)
}

/// Delete an item (stub for native testing, always succeeds).
#[cfg(not(target_arch = "wasm32"))]
pub fn delete_item(_id: &str) -> Result<(), i32> {
    Ok(())
}

/// List items (stub for native testing, always empty).
#[cfg(not(target_arch = "wasm32"))]
pub fn query_items(_query: &serde_json::Value) -> Result<Vec<crate::types::Item>, i32> {
    Ok(Vec::new())
}

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

/// The member mail request as it crosses the boundary.
#[derive(serde::Serialize)]
struct UserMailRequestWire<'a> {
    user_id: &'a str,
    subject: &'a str,
    body: &'a str,
}

/// Send a plain text message to one of the site's own members, by user id.
///
/// **The address is not a parameter, and never comes back.** The kernel
/// resolves it from the user id and the plugin never sees it. The kernel adds a
/// footer and `List-Unsubscribe` headers with a link that opts this user out
/// of this plugin's mail. Needs `mail` in `host_interfaces` **and**
/// `mail_users = true` under `[capabilities]`.
///
/// Each call is one synchronous send. To mail more than a handful of members,
/// enqueue one job per recipient and send from `tap_queue_worker`.
///
/// # Errors
///
/// [`ERR_MAIL_USERS_DENIED`](crate::host_errors::ERR_MAIL_USERS_DENIED) without
/// the capability,
/// [`ERR_MAIL_RATE_LIMITED`](crate::host_errors::ERR_MAIL_RATE_LIMITED) past the
/// plugin's or the recipient's bucket,
/// [`ERR_MAIL_RECIPIENT_UNAVAILABLE`](crate::host_errors::ERR_MAIL_RECIPIENT_UNAVAILABLE)
/// for an unknown, blocked or addressless user,
/// [`ERR_MAIL_OPTED_OUT`](crate::host_errors::ERR_MAIL_OPTED_OUT) when the user
/// unsubscribed from this plugin, and the codes `mail_send_to_site_contacts`
/// returns for a malformed message, no SMTP host, or a failed delivery.
#[cfg(target_arch = "wasm32")]
pub fn mail_send_to_user(user_id: &str, subject: &str, body: &str) -> Result<(), i32> {
    let request_json = serde_json::to_string(&UserMailRequestWire {
        user_id,
        subject,
        body,
    })
    .map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let result =
        unsafe { __mail_send_to_user(request_json.as_ptr() as i32, request_json.len() as i32) };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Send to a member (stub for native testing, always succeeds).
///
/// Still builds the request, so a plugin's own tests exercise the encoding.
#[cfg(not(target_arch = "wasm32"))]
pub fn mail_send_to_user(user_id: &str, subject: &str, body: &str) -> Result<(), i32> {
    serde_json::to_string(&UserMailRequestWire {
        user_id,
        subject,
        body,
    })
    .map(|_| ())
    .map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)
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

/// Public facts about the site's users, by id: name, status, timezone,
/// language and when the account was created. Never an address.
///
/// Requires `user_directory = true` under `[capabilities]` as well as
/// `user-api` in `host_interfaces`. Ids that name no user are left out of the
/// answer. At most 100 ids a call, in one database query, so look a batch up
/// rather than one id at a time.
///
/// # Errors
///
/// [`ERR_USER_DIRECTORY_DENIED`](crate::host_errors::ERR_USER_DIRECTORY_DENIED)
/// without the capability,
/// [`ERR_USER_IDS_INVALID`](crate::host_errors::ERR_USER_IDS_INVALID) for an id
/// that is not a user id,
/// [`ERR_USER_IDS_TOO_MANY`](crate::host_errors::ERR_USER_IDS_TOO_MANY) for more
/// than 100, and the standard negative codes for a failed read.
#[cfg(target_arch = "wasm32")]
pub fn load_users(user_ids: &[&str]) -> Result<Vec<crate::types::UserSummary>, i32> {
    let ids_json =
        serde_json::to_string(user_ids).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __load_users(
            ids_json.as_ptr() as i32,
            ids_json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    buf.truncate(result as usize);
    serde_json::from_slice(&buf).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
}

/// Look users up by id (stub for native testing; there is no directory to
/// ask, so no user is found).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_users(_user_ids: &[&str]) -> Result<Vec<crate::types::UserSummary>, i32> {
    Ok(Vec::new())
}

/// The one account named `name`: the same public facts [`load_users`]
/// answers, or `None` when no account has that name.
///
/// Matches case-insensitively, the same way the kernel's own login matches a
/// name. Requires `user_directory = true` under `[capabilities]`, the same
/// grant `load_users` needs. No status or identity filter beyond that grant:
/// a blocked account or the anonymous account answers like any other.
///
/// # Errors
///
/// [`ERR_USER_DIRECTORY_DENIED`](crate::host_errors::ERR_USER_DIRECTORY_DENIED)
/// without the capability,
/// [`ERR_USER_NAME_INVALID`](crate::host_errors::ERR_USER_NAME_INVALID) when
/// `name` is empty or longer than 255 characters, and the standard negative
/// codes for a failed read.
#[cfg(target_arch = "wasm32")]
pub fn find_user_by_name(name: &str) -> Result<Option<crate::types::UserSummary>, i32> {
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __find_user_by_name(
            name.as_ptr() as i32,
            name.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    buf.truncate(result as usize);
    serde_json::from_slice(&buf).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
}

/// Look a user up by name (stub for native testing; there is no directory to
/// ask, so no user is found).
#[cfg(not(target_arch = "wasm32"))]
pub fn find_user_by_name(_name: &str) -> Result<Option<crate::types::UserSummary>, i32> {
    Ok(None)
}

/// Up to 10 accounts whose name starts with `prefix`, sorted by name: a short
/// list an autocomplete widget can offer as a person types.
///
/// Matches case-insensitively. Requires `user_directory = true` under
/// `[capabilities]`, the same grant [`load_users`] needs, and the same
/// visibility rule as [`find_user_by_name`] applies.
///
/// # Errors
///
/// [`ERR_USER_DIRECTORY_DENIED`](crate::host_errors::ERR_USER_DIRECTORY_DENIED)
/// without the capability,
/// [`ERR_USER_NAME_PREFIX_INVALID`](crate::host_errors::ERR_USER_NAME_PREFIX_INVALID)
/// when `prefix` is empty or longer than 255 characters, and the standard
/// negative codes for a failed read.
#[cfg(target_arch = "wasm32")]
pub fn find_users_by_name_prefix(prefix: &str) -> Result<Vec<crate::types::UserSummary>, i32> {
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __find_users_by_name_prefix(
            prefix.as_ptr() as i32,
            prefix.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    buf.truncate(result as usize);
    serde_json::from_slice(&buf).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
}

/// Look users up by name prefix (stub for native testing; there is no
/// directory to ask, so no user is found).
#[cfg(not(target_arch = "wasm32"))]
pub fn find_users_by_name_prefix(_prefix: &str) -> Result<Vec<crate::types::UserSummary>, i32> {
    Ok(Vec::new())
}

/// Filter `text` through the text format `format`, exactly as the kernel does
/// when it renders a field in that format.
///
/// A plugin that renders HTML a user wrote must pass it through this before
/// returning it: the kernel does not filter a plugin's output for it. The
/// format must be one the user the plugin is acting for may use
/// (`plain_text` always; `filtered_html` with `use filtered_html`;
/// `full_html` with `use full_html`). Declare `text-format` in
/// `host_interfaces`.
///
/// # Errors
///
/// [`ERR_TEXT_FORMAT_NOT_PERMITTED`](crate::host_errors::ERR_TEXT_FORMAT_NOT_PERMITTED)
/// when the user may not use the format,
/// [`ERR_TEXT_FORMAT_UNKNOWN`](crate::host_errors::ERR_TEXT_FORMAT_UNKNOWN) for a
/// format that does not exist,
/// [`ERR_TEXT_FORMAT_TOO_LARGE`](crate::host_errors::ERR_TEXT_FORMAT_TOO_LARGE)
/// when the result is larger than the output buffer, and the standard
/// negative codes for a failed read.
#[cfg(target_arch = "wasm32")]
pub fn filter_text(text: &str, format: &str) -> Result<String, i32> {
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __filter_text(
            text.as_ptr() as i32,
            text.len() as i32,
            format.as_ptr() as i32,
            format.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    buf.truncate(result as usize);
    String::from_utf8(buf).map_err(|_| crate::host_errors::ERR_SDK_UTF8)
}

/// Create a vocabulary owned by one owner of a type this plugin declares
/// under `[[vocabulary_owners]]` (B108). Needs the `vocabulary` host
/// interface.
///
/// Returns the vocabulary, or
/// [`ERR_VOCABULARY_OWNER_UNDECLARED`](crate::host_errors::ERR_VOCABULARY_OWNER_UNDECLARED),
/// [`ERR_VOCABULARY_INVALID`](crate::host_errors::ERR_VOCABULARY_INVALID) or
/// [`ERR_VOCABULARY_EXISTS`](crate::host_errors::ERR_VOCABULARY_EXISTS).
#[cfg(target_arch = "wasm32")]
pub fn create_owned_vocabulary(
    request: &crate::types::OwnedVocabularyRequest,
) -> Result<crate::types::Vocabulary, i32> {
    let json = serde_json::to_string(request).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __create_owned_vocabulary(
            json.as_ptr() as i32,
            json.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    buf.truncate(result as usize);
    serde_json::from_slice(&buf).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
}

/// The vocabularies one owner owns, for an owner type this plugin declares
/// (B108). Needs the `vocabulary` host interface.
#[cfg(target_arch = "wasm32")]
pub fn list_owned_vocabularies(
    owner_type: &str,
    owner_id: &str,
) -> Result<Vec<crate::types::Vocabulary>, i32> {
    let mut buf = vec![0u8; MAX_OUTPUT_BUFFER];
    let result = unsafe {
        __list_owned_vocabularies(
            owner_type.as_ptr() as i32,
            owner_type.len() as i32,
            owner_id.as_ptr() as i32,
            owner_id.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    buf.truncate(result as usize);
    serde_json::from_slice(&buf).map_err(|_| crate::host_errors::ERR_SDK_DESERIALIZE)
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

/// Create an owned vocabulary (stub for native testing): there is no
/// kernel, so nothing is created.
#[cfg(not(target_arch = "wasm32"))]
pub fn create_owned_vocabulary(
    _request: &crate::types::OwnedVocabularyRequest,
) -> Result<crate::types::Vocabulary, i32> {
    Err(crate::host_errors::ERR_NO_SERVICES)
}

/// List an owner's vocabularies (stub for native testing): none.
#[cfg(not(target_arch = "wasm32"))]
pub fn list_owned_vocabularies(
    _owner_type: &str,
    _owner_id: &str,
) -> Result<Vec<crate::types::Vocabulary>, i32> {
    Ok(Vec::new())
}

/// Filter text through a text format (stub for native testing).
///
/// There is no kernel to ask, so this answers `ERR_TEXT_FORMAT_UNKNOWN` rather
/// than return the text unfiltered as if it were safe.
#[cfg(not(target_arch = "wasm32"))]
pub fn filter_text(_text: &str, _format: &str) -> Result<String, i32> {
    Err(crate::host_errors::ERR_TEXT_FORMAT_UNKNOWN)
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

// --------------------------------------------------------------------------
// Translations (`item-api`, B80)
// --------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/item-api")]
unsafe extern "C" {
    #[link_name = "list-translations"]
    fn __item_list_translations(ids_ptr: i32, ids_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "save-translation"]
    fn __item_save_translation(in_ptr: i32, in_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "delete-translation"]
    fn __item_delete_translation(id_ptr: i32, id_len: i32, lang_ptr: i32, lang_len: i32) -> i32;
}

/// The translations of up to 100 items that the user the plugin is acting for
/// may view, by item in the order given and then by language, in one call.
/// `fields` in each has the field tier applied. Needs `item-api`; from cron or a
/// queue worker, `item_background`.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Item API". More than 100
/// ids is [`crate::host_errors::ERR_PARAM_DESERIALIZE`].
#[cfg(target_arch = "wasm32")]
pub fn list_translations(item_ids: &[&str]) -> Result<Vec<crate::types::ItemTranslation>, i32> {
    let json =
        serde_json::to_string(item_ids).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    item_call(|out, max| unsafe {
        __item_list_translations(json.as_ptr() as i32, json.len() as i32, out, max)
    })
}

/// Create or replace an item's translation into one language, through the
/// kernel's item service: decided as an edit (`edit` access, or `translate
/// content` and `view`), field-gated, and then `tap_item_update` fires on the
/// other plugins and the item cache drops the item.
///
/// `translation` is `{"item_id", "language", "title", "fields"}`; `fields`
/// holds the translated values and is replaced whole. The language must be one
/// of the site's and not the item's own.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Item API":
/// [`ERR_ITEM_NOT_FOUND`](crate::host_errors::ERR_ITEM_NOT_FOUND),
/// [`ERR_ITEM_ACCESS_DENIED`](crate::host_errors::ERR_ITEM_ACCESS_DENIED), and
/// [`ERR_PARAM_DESERIALIZE`](crate::host_errors::ERR_PARAM_DESERIALIZE) for a
/// language it cannot be translated into or an empty title.
#[cfg(target_arch = "wasm32")]
pub fn save_translation(
    translation: &serde_json::Value,
) -> Result<crate::types::ItemTranslation, i32> {
    let json =
        serde_json::to_string(translation).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
    item_call(|out, max| unsafe {
        __item_save_translation(json.as_ptr() as i32, json.len() as i32, out, max)
    })
}

/// Remove an item's translation in one language, decided as
/// [`save_translation`] is. `Ok(true)` when there was one to remove.
///
/// # Errors
///
/// A host error code; see [`crate::host_errors`], "Item API".
#[cfg(target_arch = "wasm32")]
pub fn delete_translation(item_id: &str, language: &str) -> Result<bool, i32> {
    let result = unsafe {
        __item_delete_translation(
            item_id.as_ptr() as i32,
            item_id.len() as i32,
            language.as_ptr() as i32,
            language.len() as i32,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result == 1)
    }
}

/// List translations (stub for native testing, always empty).
#[cfg(not(target_arch = "wasm32"))]
pub fn list_translations(_item_ids: &[&str]) -> Result<Vec<crate::types::ItemTranslation>, i32> {
    Ok(Vec::new())
}

/// Save a translation (stub for native testing, always not found).
#[cfg(not(target_arch = "wasm32"))]
pub fn save_translation(
    _translation: &serde_json::Value,
) -> Result<crate::types::ItemTranslation, i32> {
    Err(crate::host_errors::ERR_ITEM_NOT_FOUND)
}

/// Delete a translation (stub for native testing, always nothing removed).
#[cfg(not(target_arch = "wasm32"))]
pub fn delete_translation(_item_id: &str, _language: &str) -> Result<bool, i32> {
    Ok(false)
}

/// The structured `db` calls, on the plugin's own tables.
///
/// Each is confined to the plugin's effective allowlist (the tables its
/// migrations create and its `db_tables`), under the kernel's protected floor;
/// a table outside it is [`crate::host_errors::ERR_TABLE_NOT_DECLARED`]. No
/// `raw_sql` is needed. Content is not written here: `item_translation` and the
/// item tables belong to [`crate::host::save_item`] and [`crate::host::save_translation`],
/// which decide access, fire the item taps and keep the cache right.
///
/// A `where` is a map (each key equal to its value) or an array of
/// `{"field", "op", "value"}` conditions, ANDed, with `op` one of `=`, `!=`,
/// `<`, `<=`, `>`, `>=`, `in`, `not_in`, `is_null`, `is_not_null`. An empty
/// `where`, an unknown op or an empty `in` list is refused, so none can mean
/// every row.
///
/// Values bind as JSON types: a string binds as text, so comparing a `uuid`
/// column needs the column to be text, or the comparison done in a `query`
/// definition.
pub mod db {
    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "trovato:kernel/db")]
    unsafe extern "C" {
        #[link_name = "select"]
        fn __db_select(q_ptr: i32, q_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

        #[link_name = "insert"]
        fn __db_insert(
            t_ptr: i32,
            t_len: i32,
            d_ptr: i32,
            d_len: i32,
            out_ptr: i32,
            out_max_len: i32,
        ) -> i32;

        #[link_name = "update"]
        fn __db_update(
            t_ptr: i32,
            t_len: i32,
            d_ptr: i32,
            d_len: i32,
            w_ptr: i32,
            w_len: i32,
        ) -> i64;

        #[link_name = "delete"]
        fn __db_delete(t_ptr: i32, t_len: i32, w_ptr: i32, w_len: i32) -> i64;

        #[link_name = "upsert"]
        fn __db_upsert(
            t_ptr: i32,
            t_len: i32,
            d_ptr: i32,
            d_len: i32,
            c_ptr: i32,
            c_len: i32,
            out_ptr: i32,
            out_max_len: i32,
        ) -> i32;

        #[link_name = "insert-many"]
        fn __db_insert_many(t_ptr: i32, t_len: i32, r_ptr: i32, r_len: i32) -> i64;
    }

    #[cfg(target_arch = "wasm32")]
    fn json(value: &serde_json::Value) -> Result<String, i32> {
        serde_json::to_string(value).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)
    }

    #[cfg(target_arch = "wasm32")]
    fn rows(result: i64) -> Result<u64, i32> {
        if result < 0 {
            Err(result as i32)
        } else {
            Ok(result as u64)
        }
    }

    /// `{"table", "columns", "where", "order": [{"column", "direction"}],
    /// "limit"}`; `where` here is a map. The rows, as JSON objects.
    ///
    /// # Errors
    ///
    /// A host error code; see [`crate::host_errors`], "Database".
    #[cfg(target_arch = "wasm32")]
    pub fn select(query: &serde_json::Value) -> Result<Vec<serde_json::Value>, i32> {
        let q = json(query)?;
        super::item_call(|out, max| unsafe {
            __db_select(q.as_ptr() as i32, q.len() as i32, out, max)
        })
    }

    /// Insert one row; returns it (`RETURNING *`), in a one-element array.
    ///
    /// # Errors
    ///
    /// A host error code; see [`crate::host_errors`], "Database".
    #[cfg(target_arch = "wasm32")]
    pub fn insert(table: &str, data: &serde_json::Value) -> Result<Vec<serde_json::Value>, i32> {
        let d = json(data)?;
        super::item_call(|out, max| unsafe {
            __db_insert(
                table.as_ptr() as i32,
                table.len() as i32,
                d.as_ptr() as i32,
                d.len() as i32,
                out,
                max,
            )
        })
    }

    /// Update the rows `where` matches; returns how many.
    ///
    /// # Errors
    ///
    /// A host error code; see [`crate::host_errors`], "Database".
    #[cfg(target_arch = "wasm32")]
    pub fn update(
        table: &str,
        data: &serde_json::Value,
        where_: &serde_json::Value,
    ) -> Result<u64, i32> {
        let (d, w) = (json(data)?, json(where_)?);
        rows(unsafe {
            __db_update(
                table.as_ptr() as i32,
                table.len() as i32,
                d.as_ptr() as i32,
                d.len() as i32,
                w.as_ptr() as i32,
                w.len() as i32,
            )
        })
    }

    /// Delete the rows `where` matches; returns how many.
    ///
    /// # Errors
    ///
    /// A host error code; see [`crate::host_errors`], "Database".
    #[cfg(target_arch = "wasm32")]
    pub fn delete(table: &str, where_: &serde_json::Value) -> Result<u64, i32> {
        let w = json(where_)?;
        rows(unsafe {
            __db_delete(
                table.as_ptr() as i32,
                table.len() as i32,
                w.as_ptr() as i32,
                w.len() as i32,
            )
        })
    }

    /// Insert a row, or on a conflict over `{"target": [cols]}` update it:
    /// `"update": [cols]` names the columns to change, every other column of
    /// the row when absent. Returns the row, in a one-element array.
    ///
    /// # Errors
    ///
    /// A host error code; see [`crate::host_errors`], "Database".
    #[cfg(target_arch = "wasm32")]
    pub fn upsert(
        table: &str,
        data: &serde_json::Value,
        conflict: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, i32> {
        let (d, c) = (json(data)?, json(conflict)?);
        super::item_call(|out, max| unsafe {
            __db_upsert(
                table.as_ptr() as i32,
                table.len() as i32,
                d.as_ptr() as i32,
                d.len() as i32,
                c.as_ptr() as i32,
                c.len() as i32,
                out,
                max,
            )
        })
    }

    /// Insert up to 500 rows, each naming the same columns, in one statement:
    /// all of them or none. Returns how many.
    ///
    /// # Errors
    ///
    /// A host error code; see [`crate::host_errors`], "Database". More than 500
    /// rows is [`crate::host_errors::ERR_DB_BATCH_TOO_LARGE`].
    #[cfg(target_arch = "wasm32")]
    pub fn insert_many(table: &str, rows_: &[serde_json::Value]) -> Result<u64, i32> {
        let r = serde_json::to_string(rows_).map_err(|_| crate::host_errors::ERR_SDK_SERIALIZE)?;
        rows(unsafe {
            __db_insert_many(
                table.as_ptr() as i32,
                table.len() as i32,
                r.as_ptr() as i32,
                r.len() as i32,
            )
        })
    }

    /// Select (stub for native testing, always no rows).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn select(_query: &serde_json::Value) -> Result<Vec<serde_json::Value>, i32> {
        Ok(Vec::new())
    }

    /// Insert (stub for native testing, returns the data as the row).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn insert(_table: &str, data: &serde_json::Value) -> Result<Vec<serde_json::Value>, i32> {
        Ok(vec![data.clone()])
    }

    /// Update (stub for native testing, no rows).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn update(
        _table: &str,
        _data: &serde_json::Value,
        _where: &serde_json::Value,
    ) -> Result<u64, i32> {
        Ok(0)
    }

    /// Delete (stub for native testing, no rows).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn delete(_table: &str, _where: &serde_json::Value) -> Result<u64, i32> {
        Ok(0)
    }

    /// Upsert (stub for native testing, returns the data as the row).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn upsert(
        _table: &str,
        data: &serde_json::Value,
        _conflict: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, i32> {
        Ok(vec![data.clone()])
    }

    /// Insert many (stub for native testing, counts the rows).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn insert_many(_table: &str, rows: &[serde_json::Value]) -> Result<u64, i32> {
        Ok(rows.len() as u64)
    }
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
