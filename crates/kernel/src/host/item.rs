//! Item host functions for WASM plugins.
//!
//! Provides CRUD operations for items (content entities).

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::tap::RequestState;

/// Register item host functions.
pub fn register_item_functions(linker: &mut Linker<RequestState>) -> Result<()> {
    // get_item(id) -> result<string, string>
    // Returns: length of JSON on success, negative error code on failure
    linker.func_wrap(
        "trovato:kernel/item-api",
        "get-item",
        |mut caller: wasmtime::Caller<'_, RequestState>,
         id_ptr: i32,
         id_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(m)) => m,
                _ => return -1,
            };

            let _id = match read_string_from_memory(&memory, &caller, id_ptr, id_len) {
                Ok(id) => id,
                Err(_) => return -2,
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
        |mut caller: wasmtime::Caller<'_, RequestState>,
         item_ptr: i32,
         item_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(m)) => m,
                _ => return -1,
            };

            let item_json = match read_string_from_memory(&memory, &caller, item_ptr, item_len) {
                Ok(json) => json,
                Err(_) => return -2,
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
        |mut caller: wasmtime::Caller<'_, RequestState>,
         id_ptr: i32,
         id_len: i32|
         -> i32 {
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(m)) => m,
                _ => return -1,
            };

            let _id = match read_string_from_memory(&memory, &caller, id_ptr, id_len) {
                Ok(id) => id,
                Err(_) => return -2,
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
        |mut caller: wasmtime::Caller<'_, RequestState>,
         query_ptr: i32,
         query_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(m)) => m,
                _ => return -1,
            };

            let _query = match read_string_from_memory(&memory, &caller, query_ptr, query_len) {
                Ok(q) => q,
                Err(_) => return -2,
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
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_item_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<RequestState> = Linker::new(&engine);

        let result = register_item_functions(&mut linker);
        assert!(result.is_ok());
    }
}
