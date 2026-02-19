//! Variables host functions for WASM plugins.
//!
//! Provides persistent key-value configuration storage.
//! Variables are stored in the database and cached.

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::PluginState;

/// Register variables host functions.
pub fn register_variables_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get(name, default) -> string
    // For now, returns the default value. Full implementation needs DB access.
    linker.func_wrap(
        "trovato:kernel/variables",
        "get",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         name_ptr: i32,
         name_len: i32,
         default_ptr: i32,
         default_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return 0;
            };

            // Read variable name (for logging/future use)
            let _name =
                read_string_from_memory(&memory, &caller, name_ptr, name_len).unwrap_or_default();

            // Read default value
            let default_value = read_string_from_memory(&memory, &caller, default_ptr, default_len)
                .unwrap_or_default();

            // TODO: Look up variable in database/cache
            // For now, just return the default value
            let value = default_value;

            // Write result
            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &value).unwrap_or(0)
        },
    )?;

    // set(name, value) -> result
    // Stub implementation - returns success
    linker.func_wrap(
        "trovato:kernel/variables",
        "set",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         name_ptr: i32,
         name_len: i32,
         value_ptr: i32,
         value_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let _name =
                read_string_from_memory(&memory, &caller, name_ptr, name_len).unwrap_or_default();
            let _value =
                read_string_from_memory(&memory, &caller, value_ptr, value_len).unwrap_or_default();

            // TODO: Store variable in database
            // For now, return success (0)
            0
        },
    )?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_variables_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_variables_functions(&mut linker);
        assert!(result.is_ok());
    }
}
