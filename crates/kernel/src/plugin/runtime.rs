//! WASM plugin runtime.
//!
//! Manages the Wasmtime engine, linker, and compiled plugin modules.
//! Uses a pooling allocator for efficient per-request instantiation (~5µs).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::info_parser::PluginInfo;
use crate::tap::RequestState;
use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Module, PoolingAllocationConfig,
};

/// Combined state for WASM stores, including both request state and random seed.
pub struct PluginState {
    /// Request-specific state (user context, services).
    pub request: RequestState,
}

impl PluginState {
    /// Create a new plugin state.
    pub fn new(request: RequestState) -> Self {
        Self { request }
    }
}

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
    /// Linker with host function bindings and WASI support.
    linker: Linker<PluginState>,
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

    /// Get the linker with host functions and WASI support.
    pub fn linker(&self) -> &Linker<PluginState> {
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
            .with_context(|| {
                format!(
                    "failed to read plugins directory: {}",
                    plugins_dir.display()
                )
            })?
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
                e.path().extension().is_some_and(|ext| ext == "toml")
                    && e.path()
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".info.toml"))
            })
            .collect();

        let info_path = match info_files.len() {
            0 => anyhow::bail!("no .info.toml file found in {}", plugin_dir.display()),
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
        let wasm_path = plugin_dir.join(format!("{plugin_name}.wasm"));
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
            .with_context(|| format!("failed to compile WASM module for plugin '{plugin_name}'"))?;

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

    /// Discover plugins on disk without compiling WASM.
    ///
    /// Parses each plugin's `info.toml` and returns a map of plugin name to
    /// `(PluginInfo, plugin_dir_path)`. Useful for CLI commands and startup
    /// status sync.
    pub fn discover_plugins(plugins_dir: &Path) -> HashMap<String, (PluginInfo, PathBuf)> {
        let mut discovered = HashMap::new();

        if !plugins_dir.exists() {
            info!(
                ?plugins_dir,
                "plugins directory does not exist, nothing to discover"
            );
            return discovered;
        }

        let entries = match std::fs::read_dir(plugins_dir) {
            Ok(entries) => entries,
            Err(e) => {
                warn!(error = %e, "failed to read plugins directory");
                return discovered;
            }
        };

        let mut dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        dirs.sort_by_key(|e| e.file_name());

        for entry in dirs {
            let plugin_dir = entry.path();

            // Find the .info.toml file
            let info_files: Vec<_> = match std::fs::read_dir(&plugin_dir) {
                Ok(entries) => entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().extension().is_some_and(|ext| ext == "toml")
                            && e.path()
                                .file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.ends_with(".info.toml"))
                    })
                    .collect(),
                Err(e) => {
                    warn!(dir = %plugin_dir.display(), error = %e, "failed to read plugin dir");
                    continue;
                }
            };

            let info_path = match info_files.len() {
                0 => {
                    warn!(dir = %plugin_dir.display(), "no .info.toml file found, skipping");
                    continue;
                }
                1 => info_files[0].path(),
                _ => {
                    warn!(dir = %plugin_dir.display(), "multiple .info.toml files found, skipping");
                    continue;
                }
            };

            match PluginInfo::parse(&info_path) {
                Ok(info) => {
                    let name = info.name.clone();
                    discovered.insert(name, (info, plugin_dir));
                }
                Err(e) => {
                    warn!(path = %info_path.display(), error = %e, "failed to parse plugin info");
                }
            }
        }

        discovered
    }

    /// Load only plugins whose names are in the enabled set.
    ///
    /// Similar to `load_all` but skips plugins not in the provided set.
    pub async fn load_enabled(
        &mut self,
        plugins_dir: &Path,
        enabled: &HashSet<String>,
    ) -> Result<()> {
        if !plugins_dir.exists() {
            info!(?plugins_dir, "plugins directory does not exist, skipping");
            return Ok(());
        }

        let mut entries: Vec<_> = std::fs::read_dir(plugins_dir)
            .with_context(|| {
                format!(
                    "failed to read plugins directory: {}",
                    plugins_dir.display()
                )
            })?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let plugin_dir = entry.path();

            // Peek at the dir name to see if we should bother loading
            let dir_name = entry.file_name().to_str().unwrap_or_default().to_string();

            // We need to parse info.toml to get the real name, but we can
            // skip compilation for dirs that don't match any enabled name.
            // First, try a cheap check.
            if !enabled.contains(&dir_name) {
                // The dir name might not match the plugin name in info.toml,
                // so we still need to check. Parse info.toml cheaply.
                let info_files: Vec<_> = match std::fs::read_dir(&plugin_dir) {
                    Ok(entries) => entries
                        .filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path()
                                .file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.ends_with(".info.toml"))
                        })
                        .collect(),
                    Err(_) => continue,
                };

                if info_files.len() != 1 {
                    continue;
                }

                match PluginInfo::parse(&info_files[0].path()) {
                    Ok(info) if !enabled.contains(&info.name) => {
                        debug!(plugin = %info.name, "skipping disabled plugin");
                        continue;
                    }
                    Err(_) => continue,
                    _ => {} // enabled, fall through to load
                }
            }

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

        info!(count = self.plugins.len(), "loaded enabled plugins");
        Ok(())
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

/// Creates a Linker with host function bindings and WASI support.
fn create_linker(engine: &Engine) -> Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine);

    // Add minimal WASI stubs for wasi_snapshot_preview1
    add_wasi_stubs(&mut linker)?;

    // Add custom host functions
    crate::host::register_all(&mut linker)?;

    Ok(linker)
}

/// Add minimal WASI stubs for wasi_snapshot_preview1.
///
/// These stubs allow plugins compiled for wasm32-wasip1 to run
/// without full WASI support.
fn add_wasi_stubs(linker: &mut Linker<PluginState>) -> Result<()> {
    // fd_write(fd, iovs, iovs_len, nwritten) -> errno
    // Stub that returns ENOSYS (not supported)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_write",
        |_fd: i32, _iovs: i32, _iovs_len: i32, _nwritten: i32| -> i32 {
            52 // ENOSYS
        },
    )?;

    // random_get(buf, buf_len) -> errno
    // Stub that fills buffer with pseudo-random bytes
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "random_get",
        |mut caller: wasmtime::Caller<'_, PluginState>, buf: i32, buf_len: i32| -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return 8; // EBADF
            };
            let data = memory.data_mut(&mut caller);
            let buf = buf as usize;
            let len = buf_len as usize;
            if buf + len > data.len() {
                return 21; // EFAULT
            }
            // Simple pseudo-random fill
            for i in 0..len {
                data[buf + i] = ((buf + i) as u8).wrapping_mul(31);
            }
            0 // Success
        },
    )?;

    // environ_get(environ, environ_buf) -> errno
    // Stub that returns no environment variables
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "environ_get",
        |_environ: i32, _environ_buf: i32| -> i32 {
            0 // Success (no env vars)
        },
    )?;

    // environ_sizes_get(environ_count, environ_buf_size) -> errno
    // Stub that returns 0 env vars
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "environ_sizes_get",
        |mut caller: wasmtime::Caller<'_, PluginState>, count_ptr: i32, size_ptr: i32| -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return 8; // EBADF
            };
            let data = memory.data_mut(&mut caller);
            let count_ptr = count_ptr as usize;
            let size_ptr = size_ptr as usize;
            if count_ptr + 4 > data.len() || size_ptr + 4 > data.len() {
                return 21; // EFAULT
            }
            // Write 0 for both count and size
            data[count_ptr..count_ptr + 4].copy_from_slice(&0u32.to_le_bytes());
            data[size_ptr..size_ptr + 4].copy_from_slice(&0u32.to_le_bytes());
            0 // Success
        },
    )?;

    // proc_exit(code) -> never returns
    // Stub that panics (shouldn't be called)
    linker.func_wrap("wasi_snapshot_preview1", "proc_exit", |_code: i32| {
        // Can't actually exit from a plugin
    })?;

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
