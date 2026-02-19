//! Tap dispatcher - invokes plugin taps in weight order.
//!
//! The dispatcher calls all plugins implementing a tap, collecting their results.
//! Errors are logged and skipped, allowing other plugins to continue.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, error, warn};
use wasmtime::{Instance, Store, TypedFunc};

use super::{RequestState, TapHandler, TapRegistry};
use crate::plugin::{PluginRuntime, PluginState};

/// Result from a single tap invocation.
#[derive(Debug)]
pub struct TapResult {
    /// Plugin that produced this result.
    pub plugin_name: String,
    /// JSON output from the tap.
    pub output: String,
}

/// Dispatcher for invoking taps across plugins.
pub struct TapDispatcher {
    runtime: Arc<PluginRuntime>,
    registry: Arc<TapRegistry>,
}

impl TapDispatcher {
    /// Create a new tap dispatcher.
    pub fn new(runtime: Arc<PluginRuntime>, registry: Arc<TapRegistry>) -> Self {
        Self { runtime, registry }
    }

    /// Get the tap registry for handler introspection.
    pub fn registry(&self) -> &TapRegistry {
        &self.registry
    }

    /// Dispatch a tap to all implementing plugins.
    ///
    /// Calls each plugin's tap function in weight order, collecting results.
    /// If a plugin errors, it is logged and skipped.
    ///
    /// # Arguments
    /// * `tap_name` - The tap to invoke (e.g., "tap_item_view")
    /// * `input_json` - JSON input to pass to the tap
    /// * `state` - Per-request state for the WASM Store
    ///
    /// # Returns
    /// Vector of results from each plugin, in weight order.
    pub async fn dispatch(
        &self,
        tap_name: &str,
        input_json: &str,
        state: RequestState,
    ) -> Vec<TapResult> {
        let handlers = self.registry.get_handlers(tap_name);
        if handlers.is_empty() {
            debug!(tap = %tap_name, "no handlers registered for tap");
            return Vec::new();
        }

        let mut results = Vec::with_capacity(handlers.len());

        for handler in handlers {
            match self
                .invoke_handler(tap_name, input_json, handler, state.clone())
                .await
            {
                Ok(output) => {
                    results.push(TapResult {
                        plugin_name: handler.plugin.info.name.clone(),
                        output,
                    });
                }
                Err(e) => {
                    error!(
                        plugin = %handler.plugin.info.name,
                        tap = %tap_name,
                        error = %e,
                        "tap invocation failed"
                    );
                }
            }
        }

        debug!(
            tap = %tap_name,
            handlers = handlers.len(),
            results = results.len(),
            "dispatch complete"
        );

        results
    }

    /// Dispatch a tap and expect exactly one result.
    ///
    /// Useful for taps where only one plugin should respond.
    pub async fn dispatch_one(
        &self,
        tap_name: &str,
        input_json: &str,
        state: RequestState,
    ) -> Option<TapResult> {
        let mut results = self.dispatch(tap_name, input_json, state).await;
        if results.len() > 1 {
            warn!(
                tap = %tap_name,
                count = results.len(),
                "expected single result, got multiple"
            );
        }
        results.pop()
    }

    /// Invoke a single handler.
    async fn invoke_handler(
        &self,
        tap_name: &str,
        input_json: &str,
        handler: &TapHandler,
        state: RequestState,
    ) -> Result<String> {
        let plugin = &handler.plugin;
        let engine = self.runtime.engine();

        // Create combined plugin state with WASI and request state
        let plugin_state = PluginState::new(state);

        // Create a new Store with plugin state
        let mut store = Store::new(engine, plugin_state);

        // Instantiate the module
        let instance = self
            .runtime
            .linker()
            .instantiate_async(&mut store, &plugin.module)
            .await
            .with_context(|| format!("failed to instantiate plugin '{}'", plugin.info.name))?;

        // Get the tap function export
        let func = get_tap_function(&instance, &mut store, tap_name)?;

        // Allocate input in WASM memory and call the function
        let output = call_tap_function(&instance, &mut store, func, input_json).await?;

        Ok(output)
    }
}

/// Get a tap function from a WASM instance.
fn get_tap_function(
    instance: &Instance,
    store: &mut Store<PluginState>,
    tap_name: &str,
) -> Result<TypedFunc<(i32, i32), i64>> {
    // Use tap name directly as export name (e.g., "tap_item_view" stays "tap_item_view")
    instance
        .get_typed_func::<(i32, i32), i64>(&mut *store, tap_name)
        .with_context(|| format!("tap '{tap_name}' not exported"))
}

/// Call a tap function with JSON input.
///
/// This handles the memory protocol:
/// 1. Write input JSON to WASM memory
/// 2. Call the function with ptr and len
/// 3. Read output JSON from returned ptr<<32|len
async fn call_tap_function(
    instance: &Instance,
    store: &mut Store<PluginState>,
    func: TypedFunc<(i32, i32), i64>,
    input_json: &str,
) -> Result<String> {
    let memory = instance
        .get_memory(&mut *store, "memory")
        .context("plugin missing memory export")?;

    // Simple memory protocol: write input at offset 0, output at offset 65536
    let input_offset = 0i32;
    let _output_offset = 65536i32;
    let input_bytes = input_json.as_bytes();

    // Write input to memory
    {
        let data = memory.data_mut(&mut *store);
        if input_bytes.len() > 65536 {
            anyhow::bail!("input too large: {} bytes", input_bytes.len());
        }
        data[input_offset as usize..input_offset as usize + input_bytes.len()]
            .copy_from_slice(input_bytes);
    }

    // Call the function
    let result = func
        .call_async(&mut *store, (input_offset, input_bytes.len() as i32))
        .await
        .context("tap function call failed")?;

    // Decode result: high 32 bits = ptr, low 32 bits = len
    let output_ptr = (result >> 32) as i32;
    let output_len = (result & 0xFFFFFFFF) as i32;

    if output_len < 0 {
        anyhow::bail!("tap returned error code: {output_len}");
    }

    // Read output from memory
    let output = {
        let data = memory.data(&*store);
        let start = output_ptr as usize;
        let end = start + output_len as usize;
        if end > data.len() {
            anyhow::bail!("output out of bounds: {start}..{end}");
        }
        String::from_utf8(data[start..end].to_vec()).context("invalid UTF-8 in tap output")?
    };

    Ok(output)
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::PluginConfig;
    use std::path::Path;

    #[allow(dead_code)]
    fn test_plugins_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("plugins")
    }

    #[test]
    fn dispatcher_creation() {
        let runtime = Arc::new(
            PluginRuntime::new(&PluginConfig::default()).expect("failed to create runtime"),
        );
        let registry = Arc::new(TapRegistry::from_plugins(&runtime));
        let dispatcher = TapDispatcher::new(runtime, registry);

        // Dispatcher created successfully
        assert!(dispatcher.registry.tap_count() == 0);
    }

    #[tokio::test]
    async fn dispatch_empty_tap() {
        let runtime = Arc::new(
            PluginRuntime::new(&PluginConfig::default()).expect("failed to create runtime"),
        );
        let registry = Arc::new(TapRegistry::from_plugins(&runtime));
        let dispatcher = TapDispatcher::new(runtime, registry);

        let results = dispatcher
            .dispatch("tap_nonexistent", "{}", RequestState::default())
            .await;

        assert!(results.is_empty());
    }

    #[test]
    fn registry_accessor_returns_same_registry() {
        let runtime = Arc::new(PluginRuntime::new(&PluginConfig::default()).unwrap());
        let registry = Arc::new(TapRegistry::from_plugins(&runtime));
        let dispatcher = TapDispatcher::new(runtime, registry.clone());

        assert_eq!(dispatcher.registry().tap_count(), registry.tap_count());
        assert_eq!(
            dispatcher.registry().handler_count("tap_cron"),
            registry.handler_count("tap_cron")
        );
    }
}
