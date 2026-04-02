//! Variables host functions for WASM plugins.
//!
//! Provides persistent key-value configuration storage via the
//! `site_config` table. Variable keys are namespaced by plugin name
//! to prevent collisions: `plugin.{plugin_name}.{key}`.

use anyhow::Result;
use tracing::warn;
use trovato_sdk::host_errors;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::models::SiteConfig;
use crate::plugin::PluginState;

/// Register variables host functions.
pub fn register_variables_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get(name, default) -> string (bytes written or 0)
    linker.func_wrap_async(
        "trovato:kernel/variables",
        "get",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (name_ptr, name_len, default_ptr, default_len, out_ptr, out_max_len): (
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
        )| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return 0;
                };

                let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len) else {
                    return 0;
                };

                let default_value =
                    read_string_from_memory(&memory, &caller, default_ptr, default_len)
                        .unwrap_or_default();

                // Namespace the key by plugin name
                let plugin_name = caller.data().plugin_name.clone();
                let db_key = format!("plugin.{plugin_name}.{name}");

                // Try to read from site_config via DB
                let value = if let Some(services) = caller.data().request.services() {
                    let pool = services.db.clone();
                    match SiteConfig::get(&pool, &db_key).await {
                        Ok(Some(v)) => match v {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        },
                        Ok(None) => default_value.clone(),
                        Err(e) => {
                            warn!(
                                plugin = %plugin_name,
                                key = %name,
                                error = %e,
                                "failed to read variable"
                            );
                            default_value.clone()
                        }
                    }
                } else {
                    default_value.clone()
                };

                write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &value)
                    .unwrap_or(0)
            })
        },
    )?;

    // set(name, value) -> result (0 = success, negative = error)
    linker.func_wrap_async(
        "trovato:kernel/variables",
        "set",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (name_ptr, name_len, value_ptr, value_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return host_errors::ERR_MEMORY_MISSING;
                };

                let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len) else {
                    return host_errors::ERR_PARAM1_READ;
                };

                let Ok(value) = read_string_from_memory(&memory, &caller, value_ptr, value_len)
                else {
                    return host_errors::ERR_PARAM2_OR_OUTPUT;
                };

                let Some(services) = caller.data().request.services() else {
                    return host_errors::ERR_NO_SERVICES;
                };

                let plugin_name = caller.data().plugin_name.clone();
                let db_key = format!("plugin.{plugin_name}.{name}");
                let pool = services.db.clone();

                match SiteConfig::set(&pool, &db_key, serde_json::Value::String(value)).await {
                    Ok(()) => 0,
                    Err(e) => {
                        warn!(
                            plugin = %plugin_name,
                            key = %name,
                            error = %e,
                            "failed to write variable"
                        );
                        host_errors::ERR_SQL_FAILED
                    }
                }
            })
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
        let mut config = wasmtime::Config::new();
        config.async_support(true);
        let engine = Engine::new(&config).expect("valid engine config");
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_variables_functions(&mut linker);
        assert!(result.is_ok());
    }
}
