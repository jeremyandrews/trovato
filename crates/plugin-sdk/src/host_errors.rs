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
//! - **`select(query_ptr, query_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: query read failed, `-3`: output write failed
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
//! - **`execute-raw(sql_ptr, sql_len, params_ptr, params_len) → i64`**
//!   - `-1`: memory missing, `-2`: SQL read failed, `-3`: params read failed
//!   - `≥ 0`: rows affected
//!
//! ## Item API (`trovato:item-api/*`)
//!
//! - **`get-item(id_ptr, id_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: ID read failed, `-3`: output write failed
//!   - `≥ 0`: bytes written (JSON of item)
//!
//! - **`save-item(item_ptr, item_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: item JSON read failed, `-3`: output write failed
//!   - `≥ 0`: bytes written (JSON of saved item)
//!
//! - **`delete-item(id_ptr, id_len) → i32`**
//!   - `-1`: memory missing, `-2`: ID read failed
//!   - `0`: success
//!
//! - **`query-items(query_ptr, query_len, out_ptr, out_max_len) → i32`**
//!   - `-1`: memory missing, `-2`: query JSON read failed, `-3`: output write failed
//!   - `≥ 0`: bytes written (JSON array of items)
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

/// Memory export not found — the WASM module does not export `"memory"`.
pub const ERR_MEMORY_MISSING: i32 = -1;

/// First parameter read failed — UTF-8 decoding error or out-of-bounds slice.
pub const ERR_PARAM1_READ: i32 = -2;

/// Second parameter or output write failed — buffer too small or out of bounds.
pub const ERR_PARAM2_OR_OUTPUT: i32 = -3;

/// Third parameter read failed (used by DB functions with extra params like
/// `query-raw` and `insert`).
pub const ERR_PARAM3_READ: i32 = -4;
