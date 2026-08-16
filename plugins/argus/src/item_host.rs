//! A hand-rolled binding for the kernel's `item-api` host interface.
//!
//! # Why this exists
//!
//! The kernel registers `trovato:kernel/item-api` (`crates/kernel/src/host/mod.rs`
//! → `crates/kernel/src/host/item.rs`) and `item-api` is a valid manifest
//! capability, but `crates/plugin-sdk/src/host.rs` ships **no** Rust binding for
//! it — it has externs for db, ai, http, logging, queue, crypto, variables, user
//! and plugin invocation, and nothing for items. M2 is the first code anywhere
//! that creates an Item from a plugin (M1 declared `argus_story` as a content
//! type but never wrote one), so Argus is the first consumer to hit the gap.
//!
//! The declaration below mirrors the SDK's own calling convention exactly:
//! pointer/length in, caller-allocated output buffer, byte count or a negative
//! host error code as the return. It is plugin-side only and changes nothing
//! about the frozen contract; the proper fix is an SDK binding, recorded as
//! **G-SDK-NO-ITEM** in `M2-FRICTION.md`.

use serde_json::Value;

/// Output buffer for `save-item` results, matching the SDK's own 256 KB
/// convention (`MAX_OUTPUT_BUFFER` in `crates/plugin-sdk/src/host.rs`). A saved
/// Item's JSON is far smaller than this; a full buffer means truncation, and is
/// reported as an error rather than parsed.
#[cfg(target_arch = "wasm32")]
const ITEM_OUTPUT_BUFFER: usize = 256 * 1024;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/item-api")]
unsafe extern "C" {
    #[link_name = "save-item"]
    fn __save_item(item_ptr: i32, item_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "get-item"]
    fn __get_item(id_ptr: i32, id_len: i32, out_ptr: i32, out_max_len: i32) -> i32;

    #[link_name = "query-items"]
    fn __query_items(query_ptr: i32, query_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
}

/// The shared call shape behind every binding here: hand the host a UTF-8
/// payload and a caller-allocated buffer, then parse what it wrote back.
///
/// Factored out because M3 added two more callers and the buffer/truncation
/// rules are exactly the ones that should not be re-typed per call site.
#[cfg(target_arch = "wasm32")]
fn call_host(
    f: unsafe extern "C" fn(i32, i32, i32, i32) -> i32,
    payload: &str,
) -> Result<Value, i32> {
    let mut buf = vec![0u8; ITEM_OUTPUT_BUFFER];
    // SAFETY: `payload` and `buf` are live for the duration of the call, the
    // pointers and lengths describe them exactly, and the host only reads
    // `payload` and writes at most `buf.len()` bytes into `buf` — the same
    // contract every SDK host binding relies on.
    let result = unsafe {
        f(
            payload.as_ptr() as i32,
            payload.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if result < 0 {
        return Err(result);
    }
    let len = result as usize;
    if len >= ITEM_OUTPUT_BUFFER {
        return Err(trovato_sdk::host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED);
    }
    buf.truncate(len);
    let text = String::from_utf8(buf).map_err(|_| trovato_sdk::host_errors::ERR_SDK_UTF8)?;
    serde_json::from_str(&text).map_err(|_| trovato_sdk::host_errors::ERR_SDK_DESERIALIZE)
}

/// Create or update an Item.
///
/// The kernel decides create versus update from the payload: a valid non-nil
/// `id` is an update, anything else is a create and must carry `type`. The
/// saved Item's JSON (including its `id`) comes back on success; `"null"` means
/// an update named an Item that does not exist.
///
/// # Errors
///
/// Returns the host error code (negative `i32`) on failure, or the SDK's
/// buffer/serialization codes when the payload or the response cannot be
/// handled.
#[cfg(target_arch = "wasm32")]
pub fn save_item(item: &Value) -> Result<Value, i32> {
    let payload =
        serde_json::to_string(item).map_err(|_| trovato_sdk::host_errors::ERR_SDK_SERIALIZE)?;
    call_host(__save_item, &payload)
}

/// Load one Item by id. `Value::Null` means no such Item.
///
/// # Errors
///
/// Returns the host error code (negative `i32`) on failure, or the SDK's
/// buffer/deserialization codes when the response cannot be handled.
#[cfg(target_arch = "wasm32")]
pub fn get_item(id: &str) -> Result<Value, i32> {
    call_host(__get_item, id)
}

/// List Items matching `query`, which the host reads as
/// `{"type": .., "status": .., "limit": .., "offset": ..}`.
///
/// The host clamps `limit` to 100, so a caller with more than 100 rows to read
/// must page with `offset` (see `MAX_CONFIG_PAGE` in `config_host`).
///
/// # Errors
///
/// Returns the host error code (negative `i32`) on failure, or the SDK's
/// buffer/serialization codes when the query or response cannot be handled.
#[cfg(target_arch = "wasm32")]
pub fn query_items(query: &Value) -> Result<Value, i32> {
    let payload =
        serde_json::to_string(query).map_err(|_| trovato_sdk::host_errors::ERR_SDK_SERIALIZE)?;
    call_host(__query_items, &payload)
}

/// Native stub so the plugin crate still compiles (and unit-tests) off wasm.
///
/// Follows the SDK's own native-stub convention: a benign mock rather than an
/// error, so a native test of surrounding logic is not forced to special-case
/// the host boundary. It echoes the payload with a fixed id, which is enough
/// for the shape assertions the plugin's unit tests make and is never reached
/// on the wasm target.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_item(item: &Value) -> Result<Value, i32> {
    let mut echoed = item.clone();
    if let Some(map) = echoed.as_object_mut() {
        map.entry("id")
            .or_insert_with(|| Value::String("00000000-0000-4000-8000-000000000001".to_string()));
    }
    Ok(echoed)
}

/// Native stub: no Item store off wasm, so every id resolves to "not found".
///
/// The interesting logic that sits on top of this — parsing an Item into a
/// [`crate::config_host`] config struct, coercing admin input — is pure and
/// lives in `argus-core`, where it is tested without a host.
#[cfg(not(target_arch = "wasm32"))]
pub fn get_item(_id: &str) -> Result<Value, i32> {
    Ok(Value::Null)
}

/// Native stub: no Item store off wasm, so every query is empty.
#[cfg(not(target_arch = "wasm32"))]
pub fn query_items(_query: &Value) -> Result<Value, i32> {
    Ok(Value::Array(Vec::new()))
}
