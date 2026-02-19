//! Logging host functions for WASM plugins.
//!
//! Provides structured logging from plugins to the kernel's tracing system.

use anyhow::Result;
use tracing::{debug, error, info, trace, warn};
use wasmtime::Linker;

use super::read_string_from_memory;
use crate::plugin::PluginState;

/// Register logging host functions.
pub fn register_logging_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker.func_wrap(
        "trovato:kernel/logging",
        "log",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         level_ptr: i32,
         level_len: i32,
         plugin_ptr: i32,
         plugin_len: i32,
         message_ptr: i32,
         message_len: i32| {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                error!("plugin missing memory export");
                return;
            };

            let level = read_string_from_memory(&memory, &caller, level_ptr, level_len)
                .unwrap_or_else(|_| "info".to_string());
            let plugin = read_string_from_memory(&memory, &caller, plugin_ptr, plugin_len)
                .unwrap_or_else(|_| "unknown".to_string());
            let message = read_string_from_memory(&memory, &caller, message_ptr, message_len)
                .unwrap_or_else(|_| "<invalid message>".to_string());

            match level.as_str() {
                "trace" => trace!(plugin = %plugin, "{}", message),
                "debug" => debug!(plugin = %plugin, "{}", message),
                "info" => info!(plugin = %plugin, "{}", message),
                "warn" => warn!(plugin = %plugin, "{}", message),
                "error" => error!(plugin = %plugin, "{}", message),
                _ => info!(plugin = %plugin, level = %level, "{}", message),
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_logging_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_logging_functions(&mut linker);
        assert!(result.is_ok());
    }
}
