//! Database host functions for WASM plugins.
//!
//! Provides structured database access to prevent SQL injection.
//! All queries use JSON-encoded parameters.

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::PluginState;

/// Register database host functions.
pub fn register_db_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // select(query_json) -> result<string, string>
    linker.func_wrap(
        "trovato:kernel/db",
        "select",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         query_ptr: i32,
         query_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_query) = read_string_from_memory(&memory, &caller, query_ptr, query_len) else {
                return -2;
            };

            // TODO: Implement actual database select
            // Query format: {"table": "...", "columns": [...], "where": {...}, "order": [...], "limit": n}
            let empty_results = "[]";

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, empty_results)
                .unwrap_or(-3)
        },
    )?;

    // insert(table, data_json) -> result<string, string>
    linker.func_wrap(
        "trovato:kernel/db",
        "insert",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         table_ptr: i32,
         table_len: i32,
         data_ptr: i32,
         data_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_table) = read_string_from_memory(&memory, &caller, table_ptr, table_len) else {
                return -2;
            };

            let Ok(_data) = read_string_from_memory(&memory, &caller, data_ptr, data_len) else {
                return -3;
            };

            // TODO: Implement actual database insert
            // Returns the inserted row as JSON
            let stub_result = r#"{"id":"00000000-0000-0000-0000-000000000000"}"#;

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, stub_result)
                .unwrap_or(-4)
        },
    )?;

    // update(table, data_json, where_json) -> result<u64, string>
    linker.func_wrap(
        "trovato:kernel/db",
        "update",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         table_ptr: i32,
         table_len: i32,
         data_ptr: i32,
         data_len: i32,
         where_ptr: i32,
         where_len: i32|
         -> i64 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_table) = read_string_from_memory(&memory, &caller, table_ptr, table_len) else {
                return -2;
            };

            let Ok(_data) = read_string_from_memory(&memory, &caller, data_ptr, data_len) else {
                return -3;
            };

            let Ok(_where_clause) = read_string_from_memory(&memory, &caller, where_ptr, where_len)
            else {
                return -4;
            };

            // TODO: Implement actual database update
            // Returns rows affected
            0
        },
    )?;

    // delete(table, where_json) -> result<u64, string>
    linker.func_wrap(
        "trovato:kernel/db",
        "delete",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         table_ptr: i32,
         table_len: i32,
         where_ptr: i32,
         where_len: i32|
         -> i64 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_table) = read_string_from_memory(&memory, &caller, table_ptr, table_len) else {
                return -2;
            };

            let Ok(_where_clause) = read_string_from_memory(&memory, &caller, where_ptr, where_len)
            else {
                return -3;
            };

            // TODO: Implement actual database delete
            // Returns rows affected
            0
        },
    )?;

    // query_raw(sql, params_json) -> result<string, string>
    // Note: This is a privileged operation - plugins may need explicit permission
    linker.func_wrap(
        "trovato:kernel/db",
        "query-raw",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         sql_ptr: i32,
         sql_len: i32,
         params_ptr: i32,
         params_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_sql) = read_string_from_memory(&memory, &caller, sql_ptr, sql_len) else {
                return -2;
            };

            let Ok(_params) = read_string_from_memory(&memory, &caller, params_ptr, params_len)
            else {
                return -3;
            };

            // TODO: Implement actual raw query execution with proper sandboxing
            let empty_results = "[]";

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, empty_results)
                .unwrap_or(-4)
        },
    )?;

    // execute_raw(sql, params_json) -> result<u64, string>
    linker.func_wrap(
        "trovato:kernel/db",
        "execute-raw",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         sql_ptr: i32,
         sql_len: i32,
         params_ptr: i32,
         params_len: i32|
         -> i64 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_sql) = read_string_from_memory(&memory, &caller, sql_ptr, sql_len) else {
                return -2;
            };

            let Ok(_params) = read_string_from_memory(&memory, &caller, params_ptr, params_len)
            else {
                return -3;
            };

            // TODO: Implement actual raw execute with proper sandboxing
            0
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_db_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_db_functions(&mut linker);
        assert!(result.is_ok());
    }
}
