//! Cache host functions for WASM plugins.
//!
//! Provides caching with tag-based invalidation.

use anyhow::Result;
use wasmtime::Linker;

use super::read_string_from_memory;
use crate::plugin::PluginState;

/// Register cache host functions.
pub fn register_cache_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get(bin, key) -> option<string>
    // Returns -1 if not found, or length if found
    linker.func_wrap(
        "trovato:kernel/cache-api",
        "get",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         bin_ptr: i32,
         bin_len: i32,
         key_ptr: i32,
         key_len: i32,
         _out_ptr: i32,
         _out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            let _bin =
                read_string_from_memory(&memory, &caller, bin_ptr, bin_len).unwrap_or_default();
            let _key =
                read_string_from_memory(&memory, &caller, key_ptr, key_len).unwrap_or_default();

            // TODO: Implement actual cache lookup using Redis or moka
            // For now, always return cache miss
            -1
        },
    )?;

    // set(bin, key, value, tags_json)
    linker.func_wrap(
        "trovato:kernel/cache-api",
        "set",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         bin_ptr: i32,
         bin_len: i32,
         key_ptr: i32,
         key_len: i32,
         value_ptr: i32,
         value_len: i32,
         tags_ptr: i32,
         tags_len: i32| {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return;
            };

            let _bin =
                read_string_from_memory(&memory, &caller, bin_ptr, bin_len).unwrap_or_default();
            let _key =
                read_string_from_memory(&memory, &caller, key_ptr, key_len).unwrap_or_default();
            let _value =
                read_string_from_memory(&memory, &caller, value_ptr, value_len).unwrap_or_default();
            let _tags =
                read_string_from_memory(&memory, &caller, tags_ptr, tags_len).unwrap_or_default();

            // TODO: Implement actual cache set using Redis or moka
            // For now, no-op
        },
    )?;

    // invalidate_tag(tag)
    linker.func_wrap(
        "trovato:kernel/cache-api",
        "invalidate-tag",
        |mut caller: wasmtime::Caller<'_, PluginState>, tag_ptr: i32, tag_len: i32| {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return;
            };

            let _tag =
                read_string_from_memory(&memory, &caller, tag_ptr, tag_len).unwrap_or_default();

            // TODO: Implement actual cache invalidation
            // For now, no-op
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
    fn register_cache_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_cache_functions(&mut linker);
        assert!(result.is_ok());
    }
}
