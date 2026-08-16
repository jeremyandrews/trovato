//! Tap dispatcher - invokes plugin taps in weight order.
//!
//! The dispatcher calls all plugins implementing a tap, collecting their results.
//! Errors are logged and skipped, allowing other plugins to continue.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, error, warn};
use wasmtime::{Instance, Store, TypedFunc};

use super::{RequestState, TapHandler, TapRegistry};
use crate::plugin::{CompiledPlugin, PluginRuntime, PluginState, WasmtimeExt};

/// Background tap names that may make many network or DB calls.
///
/// These receive a 150-second epoch deadline instead of the 10-second default
/// used for request-scoped taps.  **Add new long-running background taps here.**
const BACKGROUND_TAPS: &[&str] = &[
    "tap_install",
    "tap_cron",
    "tap_queue_worker",
    "tap_queue_info",
];

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

    /// Get the plugin runtime backing this dispatcher.
    ///
    /// Used by background/cron contexts to obtain the runtime handle needed to
    /// populate [`RequestServices`](super::RequestServices) with plugin-to-plugin
    /// invocation capability (FR-4a).
    pub fn runtime(&self) -> &Arc<PluginRuntime> {
        &self.runtime
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

    /// Dispatch a tap to a specific named plugin.
    ///
    /// Useful for queue worker dispatch where the kernel knows which plugin
    /// owns each queue item.  Returns `None` if the plugin does not implement
    /// the tap or is not loaded.
    pub async fn dispatch_to_plugin(
        &self,
        tap_name: &str,
        input_json: &str,
        plugin_name: &str,
        state: RequestState,
    ) -> Option<TapResult> {
        let handlers = self.registry.get_handlers(tap_name);
        let handler = handlers
            .iter()
            .find(|h| h.plugin.info.name == plugin_name)?;

        match self
            .invoke_handler(tap_name, input_json, handler, state)
            .await
        {
            Ok(output) => Some(TapResult {
                plugin_name: plugin_name.to_string(),
                output,
            }),
            Err(e) => {
                error!(
                    plugin = %plugin_name,
                    tap = %tap_name,
                    error = %e,
                    "tap invocation failed"
                );
                None
            }
        }
    }

    /// Invoke a single handler.
    async fn invoke_handler(
        &self,
        tap_name: &str,
        input_json: &str,
        handler: &TapHandler,
        state: RequestState,
    ) -> Result<String> {
        // Background taps may make many network/DB calls and need a longer epoch
        // deadline than request-scoped taps.  Add new long-running background taps
        // to BACKGROUND_TAPS so they receive the extended limit automatically.
        // The two budgets are named constants in the resource-limits home
        // (WASM-4); values are unchanged.
        let epoch_deadline = if BACKGROUND_TAPS.contains(&tap_name) {
            crate::plugin::limits::BACKGROUND_TAP_EPOCH_DEADLINE_SECS
        } else {
            crate::plugin::limits::TAP_EPOCH_DEADLINE_SECS
        };

        match instantiate_and_call_export(
            &self.runtime,
            handler.plugin.as_ref(),
            tap_name,
            input_json,
            state,
            epoch_deadline,
        )
        .await
        {
            Ok(output) => Ok(output),
            // A missing tap export is normal (a plugin may declare it in the
            // registry but not export it); preserve the historical message.
            Err(ExportCallError::ExportMissing) => {
                Err(anyhow::anyhow!("tap '{tap_name}' not exported"))
            }
            Err(ExportCallError::Failed(e)) => Err(e),
        }
    }
}

/// Outcome of resolving and calling a named WASM export through the memory
/// protocol, distinguishing a missing export from an execution failure.
///
/// Tap dispatch collapses both into a logged-and-skipped error, but FR-4a
/// `invoke` maps them to different frozen error prefixes (`function-not-exported`
/// vs `target-errored`), so the shared primitive must keep them apart.
#[derive(Debug)]
pub(crate) enum ExportCallError {
    /// The requested export is absent (or has the wrong signature) on the
    /// instantiated module.
    ExportMissing,
    /// Instantiation or execution failed (trap, memory-protocol violation, etc.).
    Failed(anyhow::Error),
}

/// Instantiate `plugin` in a fresh `Store`, resolve the named `export`, and call
/// it through the JSON memory protocol.
///
/// This is the single-`Store`-per-call primitive shared by tap dispatch
/// ([`TapDispatcher::invoke_handler`], which resolves a tap name) and
/// plugin-to-plugin invocation (`host::plugin_api::invoke`, which resolves an
/// arbitrary published function name — see FR-4a / Story 2.2). It performs no
/// permission or payload-size checks; callers enforce their own policy before and
/// after calling. `epoch_deadline` is the per-call epoch budget (seconds of CPU).
pub(crate) async fn instantiate_and_call_export(
    runtime: &PluginRuntime,
    plugin: &CompiledPlugin,
    export: &str,
    input_json: &str,
    state: RequestState,
    epoch_deadline: u64,
) -> std::result::Result<String, ExportCallError> {
    let engine = runtime.engine();

    // Create combined plugin state with WASI and request state, then a fresh
    // Store (Law 3: one Store per plugin per call).
    let limits = runtime.limits();
    let plugin_state = PluginState::with_db_policy(
        state,
        plugin.info.name.clone(),
        plugin.db_policy().clone(),
        limits,
    )
    .with_ai_background(plugin.ai_background())
    .with_http_max_transfer(plugin.http_max_transfer());
    let mut store = Store::new(engine, plugin_state);

    // Attach the per-`Store` resource limiter (WASM-4): it bounds this single
    // call's linear-memory and table growth and fails the call cleanly on breach
    // (a logged, attributed trap — never the kernel). Because the limiter lives
    // in this call's fresh `PluginState`, the bound is per-`Store`: one plugin
    // exhausting its cap cannot touch a concurrent call's budget.
    store.limiter(|state| &mut state.limiter);

    // Fuel metering (WASM-4, opt-in). Only pour fuel when the engine was built
    // with `consume_fuel` enabled; `set_fuel` errors otherwise. Off by default.
    if limits.enable_fuel {
        store
            .set_fuel(limits.fuel_limit)
            .into_anyhow()
            .with_context(|| format!("failed to set fuel for plugin '{}'", plugin.info.name))
            .map_err(ExportCallError::Failed)?;
    }

    // Set epoch deadline to prevent infinite loops. The engine's epoch is
    // incremented by a background thread every second.
    store.set_epoch_deadline(epoch_deadline);

    // Instantiate the module against the plugin's own linker (WASM-1): it exposes
    // only the host interfaces the plugin declares (deny-unless-declared).
    let instance = plugin
        .linker()
        .instantiate_async(&mut store, &plugin.module)
        .await
        .into_anyhow()
        .with_context(|| format!("failed to instantiate plugin '{}'", plugin.info.name))
        .map_err(ExportCallError::Failed)?;

    // Resolve the named export. A missing/mis-typed export is reported distinctly
    // so callers can map it to their own vocabulary.
    let Ok(func) = instance.get_typed_func::<(i32, i32), i64>(&mut store, export) else {
        return Err(ExportCallError::ExportMissing);
    };

    // Allocate input in WASM memory and call the function.
    call_export_function(&instance, &mut store, func, input_json)
        .await
        .map_err(ExportCallError::Failed)
}

/// Call a resolved export with JSON input.
///
/// This handles the memory protocol:
/// 1. Write input JSON to WASM memory
/// 2. Call the function with ptr and len
/// 3. Read output JSON from returned ptr<<32|len
async fn call_export_function(
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
        .into_anyhow()
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
    use crate::plugin::limits::ResourceLimits;
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

    // --- WASM-4 per-Store resource-limit fixtures -----------------------------

    /// A capability-free plugin that grows its linear memory in an unbounded loop.
    /// With a limiter cap below the pool slab it is stopped at the cap with a
    /// clean trap; without a cap it would run until the allocator failed opaquely.
    const GREEDY_MEMORY_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (param i32 i32) (result i64)
            (loop $l
              (drop (memory.grow (i32.const 1)))
              (br $l))
            (i64.const 0)))
    "#;

    /// A capability-free plugin that grows a funcref table in an unbounded loop.
    const GREEDY_TABLE_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (table $t (export "t") 0 funcref)
          (func (export "run") (param i32 i32) (result i64)
            (loop $l
              (drop (table.grow $t (ref.null func) (i32.const 1)))
              (br $l))
            (i64.const 0)))
    "#;

    /// A capability-free plugin that spins forever (epoch-deadline regression pin).
    const SPIN_LOOP_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (param i32 i32) (result i64)
            (loop $l (br $l))
            (i64.const 0)))
    "#;

    /// A well-behaved plugin that returns the constant JSON `{"ok":true}` (11
    /// bytes at offset 1024) via the memory protocol, allocating nothing extra.
    const BENIGN_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (data (i32.const 1024) "{\"ok\":true}")
          (func (export "run") (param i32 i32) (result i64)
            (i64.or
              (i64.shl (i64.const 1024) (i64.const 32))
              (i64.const 11))))
    "#;

    /// Build a `PluginConfig` with `default` pool sizing (64 MiB slab) but the
    /// given per-`Store` limiter overrides, so a low cap provably exercises the
    /// limiter's error path rather than the allocator's opaque failure.
    fn config_with_limits(limits: ResourceLimits) -> PluginConfig {
        PluginConfig {
            limits,
            ..PluginConfig::default()
        }
    }

    /// Write a capability-free WAT fixture into a temp plugin dir and load it into
    /// `runtime`, returning the compiled handle. The fixture imports no host
    /// interfaces, so it loads under deny-all (`capabilities: None`).
    fn load_wat_plugin(runtime: &mut PluginRuntime, name: &str, wat: &str) -> Arc<CompiledPlugin> {
        let dir = std::env::temp_dir().join(format!("trovato_wasm4_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.info.toml")),
            format!("name = \"{name}\"\ndescription = \"wasm4 fixture\"\nversion = \"1.0.0\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join(format!("{name}.wasm")), wat).unwrap();
        runtime.load_plugin(&dir).expect("load wasm4 fixture");
        std::fs::remove_dir_all(&dir).ok();
        runtime.get_plugin(name).expect("fixture loaded")
    }

    #[tokio::test]
    async fn greedy_memory_plugin_stopped_at_limiter_cap_kernel_stays_healthy() {
        // Limiter cap 2 MiB, well below the 64 MiB pool slab, so the limiter (not
        // the allocator) must be what stops the growth.
        let limits = ResourceLimits {
            max_memory_bytes: 2 * 1024 * 1024,
            ..ResourceLimits::default()
        };
        let mut runtime = PluginRuntime::new(&config_with_limits(limits)).unwrap();
        let greedy = load_wat_plugin(&mut runtime, "greedy_mem", GREEDY_MEMORY_WAT);
        let benign = load_wat_plugin(&mut runtime, "benign_after_mem", BENIGN_WAT);

        let err = match instantiate_and_call_export(
            &runtime,
            &greedy,
            "run",
            "{}",
            RequestState::default(),
            10,
        )
        .await
        {
            Err(ExportCallError::Failed(e)) => format!("{e:#}"),
            other => panic!("expected a clean limiter failure, got {other:?}"),
        };
        assert!(
            err.contains("memory-limit-exceeded"),
            "limiter error must be attributed and specific, got: {err}"
        );

        // Kernel is healthy: the very next call on the same runtime succeeds.
        let ok = instantiate_and_call_export(
            &runtime,
            &benign,
            "run",
            "{}",
            RequestState::default(),
            10,
        )
        .await
        .expect("kernel must serve the next call after a limiter trap");
        assert_eq!(ok, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn greedy_table_plugin_bounded_at_element_cap() {
        let limits = ResourceLimits {
            max_table_elements: 5,
            ..ResourceLimits::default()
        };
        let mut runtime = PluginRuntime::new(&config_with_limits(limits)).unwrap();
        let plugin = load_wat_plugin(&mut runtime, "greedy_table", GREEDY_TABLE_WAT);

        let err = match instantiate_and_call_export(
            &runtime,
            &plugin,
            "run",
            "{}",
            RequestState::default(),
            10,
        )
        .await
        {
            Err(ExportCallError::Failed(e)) => format!("{e:#}"),
            other => panic!("expected a clean table-limit failure, got {other:?}"),
        };
        assert!(
            err.contains("table-limit-exceeded"),
            "table limiter error must be attributed, got: {err}"
        );
    }

    #[tokio::test]
    async fn spin_loop_plugin_dies_at_epoch_deadline() {
        // Regression pin: the epoch mechanism still kills a pure spin loop. Bump
        // the engine epoch quickly so the 1-tick deadline trips in milliseconds
        // rather than waiting on the 1-Hz background ticker.
        let mut runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        let plugin = load_wat_plugin(&mut runtime, "spinner", SPIN_LOOP_WAT);
        let engine = runtime.engine().clone();

        let call =
            instantiate_and_call_export(&runtime, &plugin, "run", "{}", RequestState::default(), 1);
        let bump = async {
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                engine.increment_epoch();
            }
        };
        let (res, ()) = tokio::join!(call, bump);
        assert!(
            matches!(res, Err(ExportCallError::Failed(_))),
            "spin loop must be interrupted by the epoch deadline"
        );
    }

    #[tokio::test]
    async fn limits_are_per_store_concurrent_calls_independent() {
        // A greedy call hitting its cap must not affect a concurrent benign call's
        // budget: each call gets its own Store + limiter.
        let limits = ResourceLimits {
            max_memory_bytes: 2 * 1024 * 1024,
            ..ResourceLimits::default()
        };
        let mut runtime = PluginRuntime::new(&config_with_limits(limits)).unwrap();
        let greedy = load_wat_plugin(&mut runtime, "greedy_iso", GREEDY_MEMORY_WAT);
        let benign = load_wat_plugin(&mut runtime, "benign_iso", BENIGN_WAT);

        let g = instantiate_and_call_export(
            &runtime,
            &greedy,
            "run",
            "{}",
            RequestState::default(),
            10,
        );
        let b = instantiate_and_call_export(
            &runtime,
            &benign,
            "run",
            "{}",
            RequestState::default(),
            10,
        );
        let (gr, br) = tokio::join!(g, b);
        assert!(
            matches!(gr, Err(ExportCallError::Failed(_))),
            "greedy call must trap at its cap"
        );
        assert_eq!(
            br.expect("benign concurrent call must succeed unaffected"),
            "{\"ok\":true}"
        );
    }

    #[test]
    fn default_config_has_fuel_off() {
        assert!(
            !PluginConfig::default().limits.enable_fuel,
            "fuel must be off by default"
        );
    }

    #[tokio::test]
    async fn fuel_exhaustion_traps_when_enabled() {
        // Tiny fuel budget: a spin loop exhausts it and traps well before the
        // generous epoch deadline, proving fuel works when opted in.
        let limits = ResourceLimits {
            enable_fuel: true,
            fuel_limit: 10_000,
            ..ResourceLimits::default()
        };
        let mut runtime = PluginRuntime::new(&config_with_limits(limits)).unwrap();
        let plugin = load_wat_plugin(&mut runtime, "fuel_spin", SPIN_LOOP_WAT);

        let res = instantiate_and_call_export(
            &runtime,
            &plugin,
            "run",
            "{}",
            RequestState::default(),
            10,
        )
        .await;
        assert!(
            matches!(res, Err(ExportCallError::Failed(_))),
            "fuel exhaustion must trap the call"
        );
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
