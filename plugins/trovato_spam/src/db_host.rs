//! A hand-rolled binding for the kernel's structured `db` update.
//!
//! # Why this exists
//!
//! The kernel registers four structured database functions on
//! `trovato:kernel/db` — `select`, `insert`, `update`, `delete`
//! (`crates/kernel/src/host/db.rs`) — and each is gated by the WASM-2 effective
//! table allowlist. `crates/plugin-sdk/src/host.rs` binds only the two *raw* SQL
//! functions from that interface, `query-raw` and `execute-raw`, and those are
//! gated by the `raw_sql` capability instead: a plugin holding it can reach any
//! table, which the policy documentation calls the SQLI-1 surface.
//!
//! This plugin writes one column of one kernel table. Declaring `raw_sql` for
//! that would trade a checked, narrow call for an unchecked, wide one, so it
//! declares `db_tables = ["comment"]` and calls the structured `update` through
//! the binding below. The declaration mirrors the SDK's own calling convention
//! exactly and changes nothing about the frozen contract; the proper fix is an
//! SDK binding for the structured four, the same gap
//! `plugins/argus/src/item_host.rs` records for `item-api`.

use serde_json::Value;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/db")]
unsafe extern "C" {
    #[link_name = "update"]
    fn __db_update(
        table_ptr: i32,
        table_len: i32,
        data_ptr: i32,
        data_len: i32,
        where_ptr: i32,
        where_len: i32,
    ) -> i64;
}

/// Update rows in `table`, setting `data` where every column in `where_clause`
/// matches. Returns the number of rows affected.
///
/// Both maps are AND-ed equality on the host side, which is what makes a
/// compare-and-set possible without a transaction: putting the expected current
/// status in `where_clause` means the write lands only if nothing else changed it
/// first.
///
/// # Errors
///
/// The host error code (negative) when the call fails: an undeclared table, an
/// invalid identifier, an empty map, or a database error.
#[cfg(target_arch = "wasm32")]
pub fn update(table: &str, data: &Value, where_clause: &Value) -> Result<u64, i32> {
    let data_json =
        serde_json::to_string(data).map_err(|_| trovato_sdk::host_errors::ERR_SDK_SERIALIZE)?;
    let where_json = serde_json::to_string(where_clause)
        .map_err(|_| trovato_sdk::host_errors::ERR_SDK_SERIALIZE)?;

    // SAFETY: every pointer/length pair describes a live local exactly, and the
    // host only reads them — the same contract each SDK host binding relies on.
    let result = unsafe {
        __db_update(
            table.as_ptr() as i32,
            table.len() as i32,
            data_json.as_ptr() as i32,
            data_json.len() as i32,
            where_json.as_ptr() as i32,
            where_json.len() as i32,
        )
    };

    if result < 0 {
        // Host error codes are i32; the interface widens them to i64.
        return Err(result as i32);
    }

    Ok(result as u64)
}

/// Native stub so the crate compiles and unit-tests off wasm.
///
/// Reports one row updated, following the SDK's convention of a benign mock
/// rather than an error, so a native test of the surrounding logic is not forced
/// to special-case the host boundary. Never reached on the wasm target.
#[cfg(not(target_arch = "wasm32"))]
pub fn update(_table: &str, _data: &Value, _where_clause: &Value) -> Result<u64, i32> {
    Ok(1)
}
