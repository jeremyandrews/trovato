//! WASM Host Function Error Codes
//!
//! All Trovato WASM host functions follow a standard error code convention
//! for their `i32` (or `i64`) return values. Negative values indicate errors;
//! non-negative values indicate success.
//!
//! # Standard Error Codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | `-1` | Memory export not found — the WASM module does not export `"memory"` |
//! | `-2` | First parameter read failed — UTF-8 error or out-of-bounds slice |
//! | `-3` | Second parameter or output write failed — buffer too small or OOB |
//! | `-4` | Third parameter read failed (DB functions with extra params) |
//! | `≥ 0` | Success — value is bytes written, rows affected, or a boolean flag |
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
