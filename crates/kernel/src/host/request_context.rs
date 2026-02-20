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
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let Ok(key) = read_string_from_memory(&memory, &caller, key_ptr, key_len) else {
                return -1;
            };

            // Namespace key by plugin name for isolation between plugins.
            let namespaced_key = format!("{}:{key}", caller.data().plugin_name);
            let value = caller
                .data()
                .request
                .get_context(&namespaced_key)
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
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return;
            };

            let Ok(key) = read_string_from_memory(&memory, &caller, key_ptr, key_len) else {
                return;
            };

            let Ok(value) = read_string_from_memory(&memory, &caller, value_ptr, value_len) else {
                return;
            };

            // Namespace key by plugin name for isolation between plugins.
            let namespaced_key = format!("{}:{key}", caller.data().plugin_name);
            caller.data_mut().request.set_context(namespaced_key, value);
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
    fn register_request_context_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_request_context_functions(&mut linker);
        assert!(result.is_ok());
    }
}
