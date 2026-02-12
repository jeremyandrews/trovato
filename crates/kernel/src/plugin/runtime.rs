//! WASM plugin runtime.
//!
//! Manages the Wasmtime engine, linker, and compiled plugin modules.
//! Uses a pooling allocator for efficient per-request instantiation (~5µs).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Module, PoolingAllocationConfig,
};

use super::info_parser::PluginInfo;
use crate::tap::RequestState;

/// Configuration for the plugin runtime.
#[derive(Debug, Clone)]
pub struct PluginConfig {
    /// Maximum number of concurrent plugin instances (for pooling allocator).
    pub max_instances: u32,
    /// Maximum memory pages per instance (64KB per page).
    pub max_memory_pages: u64,
    /// Enable async support for async host functions.
    pub async_support: bool,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            max_instances: 1000,
            max_memory_pages: 1024, // 64MB max per instance
            async_support: true,
        }
    }
}

/// A compiled plugin ready for instantiation.
#[derive(Debug)]
pub struct CompiledPlugin {
    /// Plugin metadata from .info.toml.
    pub info: PluginInfo,
    /// Compiled WASM module.
    pub module: Module,
}

/// Plugin runtime managing the WASM engine and compiled plugins.
pub struct PluginRuntime {
    /// Wasmtime engine with pooling allocator.
    engine: Engine,
    /// Linker with host function bindings.
    linker: Linker<RequestState>,
    /// Compiled plugins indexed by name.
    plugins: HashMap<String, Arc<CompiledPlugin>>,
}

impl PluginRuntime {
    /// Create a new plugin runtime with the given configuration.
    pub fn new(config: &PluginConfig) -> Result<Self> {
        let engine = create_engine(config)?;
        let linker = create_linker(&engine)?;

        Ok(Self {
            engine,
            linker,
            plugins: HashMap::new(),
        })
    }

    /// Get the Wasmtime engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Get the linker with host functions.
    pub fn linker(&self) -> &Linker<RequestState> {
        &self.linker
    }

    /// Load all plugins from a directory.
    ///
    /// Each plugin is expected to be in a subdirectory with:
    /// - `{name}.info.toml` - plugin metadata
    /// - `{name}.wasm` - compiled WASM module
    pub async fn load_all(&mut self, plugins_dir: &Path) -> Result<()> {
        if !plugins_dir.exists() {
            info!(?plugins_dir, "plugins directory does not exist, skipping");
            return Ok(());
        }

        let mut entries: Vec<_> = std::fs::read_dir(plugins_dir)
            .with_context(|| format!("failed to read plugins directory: {}", plugins_dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        // Sort for deterministic load order
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let plugin_dir = entry.path();
            match self.load_plugin(&plugin_dir) {
                Ok(()) => {}
                Err(e) => {
                    warn!(
                        plugin_dir = %plugin_dir.display(),
                        error = %e,
                        "failed to load plugin, skipping"
                    );
                }
            }
        }

        info!(count = self.plugins.len(), "loaded plugins");
        Ok(())
    }

    /// Load a single plugin from its directory.
    pub fn load_plugin(&mut self, plugin_dir: &Path) -> Result<()> {
        // Find the .info.toml file
        let info_files: Vec<_> = std::fs::read_dir(plugin_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == "toml")
                    && e.path()
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".info.toml"))
            })
            .collect();

        let info_path = match info_files.len() {
            0 => anyhow::bail!(
                "no .info.toml file found in {}",
                plugin_dir.display()
            ),
            1 => info_files[0].path(),
            _ => anyhow::bail!(
                "multiple .info.toml files found in {}",
                plugin_dir.display()
            ),
        };

        // Parse plugin info
        let info = PluginInfo::parse(&info_path)?;
        let plugin_name = info.name.clone();

        // Find and compile WASM module
        let wasm_path = plugin_dir.join(format!("{}.wasm", plugin_name));
        if !wasm_path.exists() {
            anyhow::bail!(
                "plugin '{}' WASM file not found at {}",
                plugin_name,
                wasm_path.display()
            );
        }

        let wasm_bytes = std::fs::read(&wasm_path)
            .with_context(|| format!("failed to read WASM file: {}", wasm_path.display()))?;

        let module = Module::new(&self.engine, &wasm_bytes)
            .with_context(|| format!("failed to compile WASM module for plugin '{}'", plugin_name))?;

        debug!(
            plugin = %plugin_name,
            taps = ?info.taps.implements,
            "compiled plugin"
        );

        self.plugins.insert(
            plugin_name.clone(),
            Arc::new(CompiledPlugin { info, module }),
        );

        Ok(())
    }

    /// Get a compiled plugin by name.
    pub fn get_plugin(&self, name: &str) -> Option<Arc<CompiledPlugin>> {
        self.plugins.get(name).cloned()
    }

    /// Get all loaded plugins.
    pub fn plugins(&self) -> &HashMap<String, Arc<CompiledPlugin>> {
        &self.plugins
    }

    /// Get the number of loaded plugins.
    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }
}

/// Creates a Wasmtime Engine configured with pooling allocator.
///
/// The pooling allocator pre-allocates memory for WASM instances, reducing
/// per-request instantiation overhead to ~5µs (vs ~50µs with on-demand).
fn create_engine(config: &PluginConfig) -> Result<Engine> {
    let mut wasmtime_config = Config::new();

    // Enable async support for async host functions (db queries, etc.)
    wasmtime_config.async_support(config.async_support);

    // Configure pooling allocator for efficient per-request instantiation
    let mut pooling_config = PoolingAllocationConfig::default();
    pooling_config.total_component_instances(config.max_instances);
    pooling_config.total_memories(config.max_instances);
    pooling_config.total_tables(config.max_instances);
    pooling_config.max_memory_size(config.max_memory_pages as usize * 65536);

    wasmtime_config.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling_config));

    // Optimize for speed
    wasmtime_config.cranelift_opt_level(wasmtime::OptLevel::Speed);

    Engine::new(&wasmtime_config).context("failed to create wasmtime engine with pooling allocator")
}

/// Creates a Linker with host function bindings.
fn create_linker(engine: &Engine) -> Result<Linker<RequestState>> {
    let mut linker = Linker::new(engine);
    crate::host::register_all(&mut linker)?;
    Ok(linker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_runtime_with_default_config() {
        let runtime = PluginRuntime::new(&PluginConfig::default());
        assert!(runtime.is_ok());
    }

    #[test]
    fn create_runtime_with_custom_config() {
        let config = PluginConfig {
            max_instances: 500,
            max_memory_pages: 512,
            async_support: false,
        };
        let runtime = PluginRuntime::new(&config);
        assert!(runtime.is_ok());
    }
}
