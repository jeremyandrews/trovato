//! Item host functions for WASM plugins.
//!
//! Provides CRUD operations for items (content records).

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::PluginState;

/// Register item host functions.
pub fn register_item_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get_item(id) -> result<string, string>
    // Returns: length of JSON on success, negative error code on failure
    linker.func_wrap(
        "trovato:kernel/item-api",
        "get-item",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         id_ptr: i32,
         id_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_id) = read_string_from_memory(&memory, &caller, id_ptr, id_len) else {
                return -2;
            };

            // TODO: Implement actual item lookup from database
            // For now, return a stub item
            let stub_item = r#"{"id":"00000000-0000-0000-0000-000000000000","item_type":"stub","title":"Stub Item","fields":{},"status":1,"author_id":"00000000-0000-0000-0000-000000000000","created":0,"changed":0}"#;

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, stub_item)
                .unwrap_or(-3)
        },
    )?;

    // save_item(item_json) -> result<string, string>
    // Returns: length of saved item JSON on success, negative on failure
    linker.func_wrap(
        "trovato:kernel/item-api",
        "save-item",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         item_ptr: i32,
         item_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(item_json) = read_string_from_memory(&memory, &caller, item_ptr, item_len)
            else {
                return -2;
            };

            // TODO: Implement actual item save to database
            // For now, just echo back the input
            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &item_json)
                .unwrap_or(-3)
        },
    )?;

    // delete_item(id) -> result<_, string>
    // Returns: 0 on success, negative on failure
    linker.func_wrap(
        "trovato:kernel/item-api",
        "delete-item",
        |mut caller: wasmtime::Caller<'_, PluginState>, id_ptr: i32, id_len: i32| -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(_id) = read_string_from_memory(&memory, &caller, id_ptr, id_len) else {
                return -2;
            };

            // TODO: Implement actual item deletion
            // For now, return success
            0
        },
    )?;

    // query_items(query_json) -> result<string, string>
    // Returns: length of results JSON on success, negative on failure
    linker.func_wrap(
        "trovato:kernel/item-api",
        "query-items",
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

            // TODO: Implement actual item query
            // For now, return empty array
            let empty_results = "[]";

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, empty_results)
                .unwrap_or(-3)
        },
    )?;

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_item_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_item_functions(&mut linker);
        assert!(result.is_ok());
    }
}
