//! Request context host functions for WASM plugins.
//!
//! Provides per-request key-value storage for plugin communication.

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::PluginState;

/// Register request context host functions.
pub fn register_request_context_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get(key) -> option<string>
    // Returns -1 if not found, or length if found
    linker.func_wrap(
        "trovato:kernel/request-context",
        "get",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         key_ptr: i32,
         key_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(m)) => m,
                _ => return -1,
            };

            let key = match read_string_from_memory(&memory, &caller, key_ptr, key_len) {
                Ok(k) => k,
                Err(_) => return -1,
            };

            // Get the value and clone it to avoid borrow issues
            let value = caller
                .data()
                .request
                .get_context(&key)
                .map(|s| s.to_string());

            match value {
                Some(v) => write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &v)
                    .unwrap_or(-1),
                None => -1,
            }
        },
    )?;

    // set(key, value)
    linker.func_wrap(
        "trovato:kernel/request-context",
        "set",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         key_ptr: i32,
         key_len: i32,
         value_ptr: i32,
         value_len: i32| {
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(m)) => m,
                _ => return,
            };

            let key = match read_string_from_memory(&memory, &caller, key_ptr, key_len) {
                Ok(k) => k,
                Err(_) => return,
            };

            let value = match read_string_from_memory(&memory, &caller, value_ptr, value_len) {
                Ok(v) => v,
                Err(_) => return,
            };

            caller.data_mut().request.set_context(key, value);
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_request_context_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_request_context_functions(&mut linker);
        assert!(result.is_ok());
    }
}
