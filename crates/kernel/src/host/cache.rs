//! Cache host functions for WASM plugins.
//!
//! Provides a two-tier cache (Moka L1 + Redis L2) accessible from WASM
//! plugins. Cache keys are namespaced by plugin name and bin to prevent
//! collisions: `plugin:{plugin_name}:{bin}:{key}`.
//!
//! When no cache service is available (background/test contexts), `get`
//! returns a cache miss and `set`/`invalidate-tag` are silent no-ops.

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::{PluginState, WasmtimeExt};

/// Default TTL for plugin cache entries (5 minutes).
const DEFAULT_TTL_SECS: u64 = 300;

/// Register cache host functions.
pub fn register_cache_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get(bin, key, out) -> i32 (bytes written or -1 for miss)
    linker
        .func_wrap_async(
            "trovato:kernel/cache-api",
            "get",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (bin_ptr, bin_len, key_ptr, key_len, out_ptr, out_max_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return -1;
                    };

                    let Ok(bin) = read_string_from_memory(&memory, &caller, bin_ptr, bin_len)
                    else {
                        return -1;
                    };

                    let Ok(key) = read_string_from_memory(&memory, &caller, key_ptr, key_len)
                    else {
                        return -1;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return -1; // Cache miss — no services
                    };

                    let Some(ref cache) = services.cache else {
                        return -1; // Cache miss — no cache layer
                    };

                    let plugin_name = &caller.data().plugin_name;
                    let cache_key = format!("plugin:{plugin_name}:{bin}:{key}");

                    match cache.get(&cache_key).await {
                        Some(value) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &value,
                        )
                        .unwrap_or(-1),
                        None => -1,
                    }
                })
            },
        )
        .into_anyhow()?;

    // set(bin, key, value, tags_json) -> void
    linker
        .func_wrap_async(
            "trovato:kernel/cache-api",
            "set",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (bin_ptr, bin_len, key_ptr, key_len, value_ptr, value_len, tags_ptr, tags_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return;
                    };

                    let Ok(bin) = read_string_from_memory(&memory, &caller, bin_ptr, bin_len)
                    else {
                        return;
                    };
                    let Ok(key) = read_string_from_memory(&memory, &caller, key_ptr, key_len)
                    else {
                        return;
                    };
                    let Ok(value) = read_string_from_memory(&memory, &caller, value_ptr, value_len)
                    else {
                        return;
                    };
                    let tags_json = read_string_from_memory(&memory, &caller, tags_ptr, tags_len)
                        .unwrap_or_default();

                    let Some(services) = caller.data().request.services() else {
                        return;
                    };
                    let Some(ref cache) = services.cache else {
                        return;
                    };

                    let plugin_name = &caller.data().plugin_name;
                    let cache_key = format!("plugin:{plugin_name}:{bin}:{key}");

                    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
                    let prefixed_tags: Vec<String> = tags
                        .iter()
                        .map(|t| format!("plugin:{plugin_name}:{t}"))
                        .collect();
                    let tag_refs: Vec<&str> = prefixed_tags.iter().map(|s| s.as_str()).collect();

                    cache
                        .set(&cache_key, &value, DEFAULT_TTL_SECS, &tag_refs)
                        .await;
                })
            },
        )
        .into_anyhow()?;

    // invalidate-tag(tag) -> void
    linker
        .func_wrap_async(
            "trovato:kernel/cache-api",
            "invalidate-tag",
            |mut caller: wasmtime::Caller<'_, PluginState>, (tag_ptr, tag_len): (i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return;
                    };

                    let Ok(tag) = read_string_from_memory(&memory, &caller, tag_ptr, tag_len)
                    else {
                        return;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return;
                    };
                    let Some(ref cache) = services.cache else {
                        return;
                    };

                    let plugin_name = &caller.data().plugin_name;
                    let prefixed_tag = format!("plugin:{plugin_name}:{tag}");

                    cache.invalidate_tag(&prefixed_tag).await;
                })
            },
        )
        .into_anyhow()?;

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
        let engine = Engine::new(&config).expect("valid engine config");
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_cache_functions(&mut linker);
        assert!(result.is_ok());
    }
}
