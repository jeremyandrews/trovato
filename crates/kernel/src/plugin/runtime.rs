//! WASM plugin runtime.
//!
//! Manages the Wasmtime engine, linker, and compiled plugin modules.
//! Uses a pooling allocator for efficient per-request instantiation (~5µs).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::db_policy::DbPolicy;
use super::info_parser::PluginInfo;
use super::limits::{PluginResourceLimiter, ResourceLimits};
use crate::tap::RequestState;
use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Module, PoolingAllocationConfig,
};

/// Extension trait to convert `Result<T, wasmtime::Error>` to `anyhow::Result<T>`.
///
/// Wasmtime v43+ uses a custom error type that doesn't implement
/// `std::error::Error`, so `anyhow::Context` doesn't work directly on
/// wasmtime results. This trait bridges the gap.
pub(crate) trait WasmtimeExt<T> {
    /// Convert a wasmtime result to an anyhow result.
    fn into_anyhow(self) -> anyhow::Result<T>;
}

impl<T> WasmtimeExt<T> for std::result::Result<T, wasmtime::Error> {
    fn into_anyhow(self) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::anyhow!("{e:#}"))
    }
}

/// Combined state for WASM stores, including both request state and random seed.
pub struct PluginState {
    /// Request-specific state (user context, services).
    pub request: RequestState,
    /// Plugin name (used to namespace per-plugin context keys).
    pub plugin_name: String,
    /// Effective database-scoping policy for this plugin (WASM-2 / D-19):
    /// the table allowlist enforced on structured `db` calls and the `raw_sql`
    /// gate on `query-raw`/`execute-raw`. Cloned from the plugin's
    /// [`CompiledPlugin::db_policy`] at instantiation so the `db` host functions
    /// can enforce it per call. Defaults (via [`PluginState::new`]) to an empty
    /// deny-all policy for contexts that instantiate without a compiled plugin
    /// (e.g. linker-probe tests).
    pub db_policy: Arc<DbPolicy>,
    /// Per-`Store` resource limiter (WASM-4): bounds this call's linear-memory
    /// and table growth. Attached to the `Store` at creation via
    /// `store.limiter(|s| &mut s.limiter)`; on breach it fails the call with a
    /// clean, logged, attributed trap (never the kernel). Defaults (via
    /// [`PluginState::new`]) to [`ResourceLimits::default`].
    pub limiter: PluginResourceLimiter,
    /// Whether this plugin declared the `ai_background` manifest capability
    /// (P11c / D-41). Read once at load from the plugin's `[capabilities]` and
    /// carried per call so the `ai-request` host function can authorize a
    /// background-principal AI call for a capable plugin. Defaults to `false`
    /// (deny) for the constructors used by linker/instantiation probes; the
    /// production dispatch path sets it via [`PluginState::with_ai_background`].
    pub ai_background: bool,
    /// Effective total-transfer ceiling (bytes) for this plugin's streaming HTTP
    /// fetches (`http-open`/`http-read`, P11e / D-50). Already clamped to
    /// `[1, 16 MB]` by [`CompiledPlugin::http_max_transfer`]; the streaming host
    /// functions enforce it directly. Defaults to the 1 MB
    /// `crate::host::http::DEFAULT_TRANSFER_CEILING` for the probe constructors; the
    /// production dispatch path sets it via [`PluginState::with_http_max_transfer`].
    pub http_max_transfer: u64,
    /// Open streaming-HTTP handles for this call, keyed by handle id. Lives in the
    /// per-call [`PluginState`], so handles are **Store-scoped** (P11e / D-49):
    /// they cannot leak across tap invocations, and a fresh call starts with none.
    http_streams: HashMap<u32, crate::host::http::HttpStream>,
    /// Monotonic handle-id source for [`Self::http_streams`]. Never reused within a
    /// call, so a closed handle's id cannot silently rebind to a new stream.
    next_http_handle: u32,
}

impl PluginState {
    /// Create a new plugin state with an empty deny-all database policy.
    ///
    /// Used by linker/instantiation probes that never exercise the `db` host
    /// functions. The production dispatch path uses [`PluginState::with_db_policy`]
    /// to carry the plugin's real effective allowlist.
    pub fn new(request: RequestState, plugin_name: String) -> Self {
        Self::with_db_policy(
            request,
            plugin_name,
            Arc::new(DbPolicy::default()),
            ResourceLimits::default(),
        )
    }

    /// Create a new plugin state carrying the plugin's effective DB policy and
    /// the per-`Store` resource bounds for this call.
    ///
    /// `limits` is the resolved [`ResourceLimits`] (from
    /// [`PluginRuntime::limits`]); it is stamped into the [`PluginResourceLimiter`]
    /// stored in [`Self::limiter`], which the dispatcher attaches to the `Store`.
    pub fn with_db_policy(
        request: RequestState,
        plugin_name: String,
        db_policy: Arc<DbPolicy>,
        limits: ResourceLimits,
    ) -> Self {
        let limiter = PluginResourceLimiter::new(limits, plugin_name.clone());
        Self {
            request,
            plugin_name,
            db_policy,
            limiter,
            ai_background: false,
            http_max_transfer: crate::host::http::DEFAULT_TRANSFER_CEILING,
            http_streams: HashMap::new(),
            next_http_handle: 1,
        }
    }

    /// Record whether this plugin holds the `ai_background` manifest capability
    /// (P11c / D-41). Builder-style: the production dispatch path sets it from the
    /// compiled plugin's manifest so the `ai-request` host function can authorize
    /// a background-principal AI call. Left `false` (deny) on probe contexts.
    #[must_use]
    pub fn with_ai_background(mut self, ai_background: bool) -> Self {
        self.ai_background = ai_background;
        self
    }

    /// Record this plugin's effective streaming total-transfer ceiling (P11e /
    /// D-50). Builder-style: the production dispatch path sets it from
    /// [`CompiledPlugin::http_max_transfer`] (already kernel-clamped). Left at the
    /// 1 MB default on probe contexts.
    #[must_use]
    pub fn with_http_max_transfer(mut self, max_transfer: u64) -> Self {
        self.http_max_transfer = max_transfer;
        self
    }

    /// Register a newly-opened streaming HTTP stream and return its handle id, or
    /// `None` if this call already holds [`crate::host::http::MAX_OPEN_HTTP_STREAMS`] open handles
    /// (P11e / D-49). The id is monotonic within the call and never reused.
    pub(crate) fn http_stream_insert(
        &mut self,
        stream: crate::host::http::HttpStream,
    ) -> Option<u32> {
        if self.http_streams.len() >= crate::host::http::MAX_OPEN_HTTP_STREAMS {
            return None;
        }
        let handle = self.next_http_handle;
        self.next_http_handle = self.next_http_handle.wrapping_add(1);
        self.http_streams.insert(handle, stream);
        Some(handle)
    }

    /// Borrow an open streaming HTTP stream by handle, or `None` if the handle is
    /// unknown or already closed (P11e / D-49).
    pub(crate) fn http_stream_get(
        &mut self,
        handle: u32,
    ) -> Option<&mut crate::host::http::HttpStream> {
        self.http_streams.get_mut(&handle)
    }

    /// Close and drop a streaming HTTP handle. Returns `true` if the handle was
    /// open, `false` if it was unknown or already closed (P11e / D-49).
    pub(crate) fn http_stream_close(&mut self, handle: u32) -> bool {
        self.http_streams.remove(&handle).is_some()
    }
}

/// Configuration for the plugin runtime.
///
/// Holds two coherent layers of memory bounding: the pooling-allocator slab
/// ([`Self::max_memory_pages`], the hard backstop) and the per-`Store`
/// [`ResourceLimits`] (the effective, cleanly-erroring caps). Load from the
/// environment with [`PluginConfig::from_env`](crate::config); defaults are the
/// values below, and the two memory layers default equal (64 MiB).
#[derive(Debug, Clone)]
pub struct PluginConfig {
    /// Maximum number of concurrent plugin instances (for pooling allocator).
    pub max_instances: u32,
    /// Maximum memory pages per instance (64KB per page). Sizes the pooling
    /// allocator's per-instance slab, the hard backstop behind
    /// [`ResourceLimits::max_memory_bytes`]; kept ≥ the limiter cap at engine
    /// creation.
    pub max_memory_pages: u64,
    /// Per-`Store` resource bounds (WASM-4): linear-memory / table caps and the
    /// optional fuel budget, attached to every plugin `Store` at creation.
    pub limits: ResourceLimits,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            max_instances: 1000,
            max_memory_pages: 1024, // 64MB max per instance
            limits: ResourceLimits::default(),
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
    /// Modification time of the `.wasm` file at load time.
    pub mtime: Option<std::time::SystemTime>,
    /// Per-plugin linker exposing exactly the host interfaces this plugin
    /// declares under `[capabilities] host_interfaces` (WASM-1,
    /// deny-unless-declared), plus the baseline WASI stubs. Built once at load
    /// (`build_plugin_linker`) and reused for every instantiation — never
    /// rebuilt on the dispatch/`invoke` hot path (OQ-11 budget).
    linker: Linker<PluginState>,
    /// Effective database-scoping policy (WASM-2 / D-19): the table allowlist
    /// (migration-owned ∪ manifest `db_tables`) and the `raw_sql` gate. Derived
    /// once at load (`DbPolicy::derive`) and cloned into each request's
    /// [`PluginState`] — never recomputed on the dispatch hot path.
    db_policy: Arc<DbPolicy>,
}

impl CompiledPlugin {
    /// The per-plugin linker bound at instantiation time.
    ///
    /// Exposes only this plugin's declared host interfaces (deny-unless-declared)
    /// plus baseline WASI stubs.
    pub fn linker(&self) -> &Linker<PluginState> {
        &self.linker
    }

    /// This plugin's effective database-scoping policy (WASM-2 / D-19).
    pub fn db_policy(&self) -> &Arc<DbPolicy> {
        &self.db_policy
    }

    /// Whether this plugin declared the `ai_background` manifest capability
    /// (P11c / D-41) — the gate for `ai-request` from a background dispatch
    /// context under the kernel-internal background principal. Absent
    /// `[capabilities]` yields `false` (deny), matching the deny-by-default
    /// posture of the other manifest capabilities.
    pub fn ai_background(&self) -> bool {
        self.info
            .capabilities
            .as_ref()
            .is_some_and(|c| c.ai_background)
    }

    /// This plugin's effective streaming total-transfer ceiling in bytes (P11e /
    /// D-50). The manifest-declared `http_max_transfer` clamped to the kernel range
    /// `[1, 16 MB]`; absent `[capabilities]` or an absent field yields the 1 MB
    /// default. Delegates to `crate::host::http::clamp_transfer_ceiling` (the single
    /// home of the policy) so a manifest can never grant more than the kernel
    /// maximum.
    pub fn http_max_transfer(&self) -> u64 {
        crate::host::http::clamp_transfer_ceiling(
            self.info
                .capabilities
                .as_ref()
                .and_then(|c| c.http_max_transfer),
        )
    }
}

/// A plugin that failed to load.
#[derive(Debug, Clone)]
pub struct PluginLoadError {
    /// Plugin directory or name.
    pub plugin: String,
    /// Error description.
    pub error: String,
}

/// Plugin runtime managing the WASM engine and compiled plugins.
pub struct PluginRuntime {
    /// Wasmtime engine with pooling allocator.
    engine: Engine,
    /// Compiled plugins indexed by name. Each carries its own per-plugin linker
    /// (WASM-1) — there is no shared all-interfaces linker.
    plugins: HashMap<String, Arc<CompiledPlugin>>,
    /// Plugins that failed to load (for admin UI visibility).
    load_errors: Vec<PluginLoadError>,
    /// Resolved per-`Store` resource bounds (WASM-4), copied from the
    /// [`PluginConfig`] at construction and stamped into each call's
    /// [`PluginState::limiter`] on the dispatch hot path.
    limits: ResourceLimits,
}

impl PluginRuntime {
    /// Create a new plugin runtime with the given configuration.
    pub fn new(config: &PluginConfig) -> Result<Self> {
        let engine = create_engine(config)?;

        // Spawn background thread to increment the engine epoch once per second.
        // This drives epoch-based interruption: plugins with a deadline of N
        // are interrupted after ~N seconds of CPU time.
        let epoch_engine = engine.clone();
        std::thread::Builder::new()
            .name("wasm-epoch".to_string())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    epoch_engine.increment_epoch();
                }
            })
            .context("failed to spawn epoch increment thread")?;

        Ok(Self {
            engine,
            plugins: HashMap::new(),
            load_errors: Vec::new(),
            limits: config.limits,
        })
    }

    /// Get the Wasmtime engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The resolved per-`Store` resource bounds (WASM-4) for this runtime.
    ///
    /// Read on the dispatch hot path to build each call's per-`Store`
    /// [`PluginResourceLimiter`] and (when enabled) pour the call's fuel budget.
    /// Cheap `Copy`.
    pub fn limits(&self) -> ResourceLimits {
        self.limits
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

        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(plugins_dir).await.with_context(|| {
            format!(
                "failed to read plugins directory: {}",
                plugins_dir.display()
            )
        })?;
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            if entry.path().is_dir() {
                entries.push(entry);
            }
        }

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

        let mtime = std::fs::metadata(&wasm_path)
            .ok()
            .and_then(|m| m.modified().ok());

        let module = Module::new(&self.engine, &wasm_bytes)
            .into_anyhow()
            .with_context(|| format!("failed to compile WASM module for plugin '{plugin_name}'"))?;

        // Build the per-plugin linker (WASM-1): exposes only the host interfaces
        // this plugin declares, after a load-time import-vs-declaration pre-check.
        let linker = build_plugin_linker(&self.engine, &module, &info)?;

        // Derive the effective DB-scoping policy (WASM-2): migration-owned tables
        // ∪ manifest db_tables, plus the raw_sql gate.
        let db_policy = Arc::new(DbPolicy::derive(&info, plugin_dir));

        debug!(
            plugin = %plugin_name,
            taps = ?info.taps.implements,
            "compiled plugin"
        );

        self.plugins.insert(
            plugin_name.clone(),
            Arc::new(CompiledPlugin {
                info,
                module,
                mtime,
                linker,
                db_policy,
            }),
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

    /// Get the list of plugins that failed to load.
    pub fn load_errors(&self) -> &[PluginLoadError] {
        &self.load_errors
    }

    /// Discover plugins across the whole plugin search path.
    ///
    /// Directories are scanned in order and a later directory wins on a name
    /// collision, so an application can ship a plugin that shadows one of the
    /// same name earlier on the path. Shadowing is logged, because silently
    /// running a different plugin than the operator expects is the kind of
    /// thing that only surfaces at 3am.
    pub async fn discover_plugins(
        plugins_dirs: &[PathBuf],
    ) -> HashMap<String, (PluginInfo, PathBuf)> {
        let mut discovered: HashMap<String, (PluginInfo, PathBuf)> = HashMap::new();

        for dir in plugins_dirs {
            for (name, (info, plugin_dir)) in Self::discover_plugins_in(dir).await {
                if let Some((_, previous_dir)) = discovered.get(&name) {
                    warn!(
                        plugin = %name,
                        shadowed = %previous_dir.display(),
                        used = %plugin_dir.display(),
                        "plugin name found in more than one plugins directory; \
                         the later directory on the search path wins"
                    );
                }
                discovered.insert(name, (info, plugin_dir));
            }
        }

        discovered
    }

    /// Discover plugins in a single directory without compiling WASM.
    ///
    /// Parses each plugin's `info.toml` and returns a map of plugin name to
    /// `(PluginInfo, plugin_dir_path)`. Useful for CLI commands and startup
    /// status sync.
    pub async fn discover_plugins_in(plugins_dir: &Path) -> HashMap<String, (PluginInfo, PathBuf)> {
        let mut discovered = HashMap::new();

        if !plugins_dir.exists() {
            info!(
                ?plugins_dir,
                "plugins directory does not exist, nothing to discover"
            );
            return discovered;
        }

        let mut read_dir = match tokio::fs::read_dir(plugins_dir).await {
            Ok(rd) => rd,
            Err(e) => {
                warn!(error = %e, "failed to read plugins directory");
                return discovered;
            }
        };

        let mut dirs = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            if entry.path().is_dir() {
                dirs.push(entry);
            }
        }

        dirs.sort_by_key(|e| e.file_name());

        for entry in dirs {
            let plugin_dir = entry.path();

            // Find the .info.toml file
            let mut inner_read_dir = match tokio::fs::read_dir(&plugin_dir).await {
                Ok(rd) => rd,
                Err(e) => {
                    warn!(dir = %plugin_dir.display(), error = %e, "failed to read plugin dir");
                    continue;
                }
            };

            let mut info_files = Vec::new();
            while let Ok(Some(inner_entry)) = inner_read_dir.next_entry().await {
                let path = inner_entry.path();
                if path.extension().is_some_and(|ext| ext == "toml")
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".info.toml"))
                {
                    info_files.push(path);
                }
            }

            let info_path = match info_files.len() {
                0 => {
                    warn!(dir = %plugin_dir.display(), "no .info.toml file found, skipping");
                    continue;
                }
                1 => info_files[0].clone(),
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
        plugins_dirs: &[PathBuf],
        enabled: &HashSet<String>,
    ) -> Result<()> {
        // Phase 1: Determine which plugin directories to load (skip disabled).
        //
        // Discovery already walks the whole search path and resolves name
        // collisions, so this works from its output rather than re-reading the
        // directories. A shadowed plugin therefore gets loaded once, from the
        // winning directory, instead of twice.
        let discovered = Self::discover_plugins(plugins_dirs).await;

        let mut dirs_to_load: Vec<PathBuf> = discovered
            .into_iter()
            .filter_map(|(name, (_, plugin_dir))| {
                if enabled.contains(&name) {
                    Some(plugin_dir)
                } else {
                    debug!(plugin = %name, "skipping disabled plugin");
                    None
                }
            })
            .collect();

        // HashMap iteration order is arbitrary; sort so plugin load order is
        // reproducible across runs.
        dirs_to_load.sort();

        // Phase 2: Compile WASM modules concurrently using blocking tasks.
        // Module::new() is CPU-bound so we use spawn_blocking for each plugin.
        let engine = self.engine.clone();
        let mut compile_handles = Vec::new();

        for plugin_dir in dirs_to_load {
            let engine = engine.clone();
            let dir = plugin_dir.clone();
            let handle =
                tokio::task::spawn_blocking(move || compile_plugin_from_dir(&engine, &dir));
            compile_handles.push((plugin_dir, handle));
        }

        // Phase 3: Collect results, store successes and errors.
        for (plugin_dir, handle) in compile_handles {
            match handle.await {
                Ok(Ok((name, compiled))) => {
                    self.plugins.insert(name, compiled);
                }
                Ok(Err(e)) => {
                    let dir_str = plugin_dir.display().to_string();
                    warn!(plugin_dir = %dir_str, error = %e, "failed to load plugin, skipping");
                    self.load_errors.push(PluginLoadError {
                        plugin: dir_str,
                        error: format!("{e:#}"),
                    });
                }
                Err(e) => {
                    let dir_str = plugin_dir.display().to_string();
                    warn!(plugin_dir = %dir_str, error = %e, "plugin compilation task panicked");
                    self.load_errors.push(PluginLoadError {
                        plugin: dir_str,
                        error: format!("compilation task panicked: {e}"),
                    });
                }
            }
        }

        info!(
            loaded = self.plugins.len(),
            failed = self.load_errors.len(),
            "loaded enabled plugins"
        );
        Ok(())
    }
}

/// Compile a single plugin from its directory (runs on a blocking thread).
///
/// Reads the `.info.toml` and `.wasm` files, compiles the module, and
/// returns the plugin name and compiled module.
fn compile_plugin_from_dir(
    engine: &Engine,
    plugin_dir: &Path,
) -> Result<(String, Arc<CompiledPlugin>)> {
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

    let info = PluginInfo::parse(&info_path)?;
    let plugin_name = info.name.clone();

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

    let mtime = std::fs::metadata(&wasm_path)
        .ok()
        .and_then(|m| m.modified().ok());

    let module = Module::new(engine, &wasm_bytes)
        .into_anyhow()
        .with_context(|| format!("failed to compile WASM module for plugin '{plugin_name}'"))?;

    // Build the per-plugin linker (WASM-1): exposes only the host interfaces
    // this plugin declares, after a load-time import-vs-declaration pre-check.
    let linker = build_plugin_linker(engine, &module, &info)?;

    // Derive the effective DB-scoping policy (WASM-2): migration-owned tables
    // ∪ manifest db_tables, plus the raw_sql gate.
    let db_policy = Arc::new(DbPolicy::derive(&info, plugin_dir));

    debug!(
        plugin = %plugin_name,
        taps = ?info.taps.implements,
        "compiled plugin"
    );

    Ok((
        plugin_name,
        Arc::new(CompiledPlugin {
            info,
            module,
            mtime,
            linker,
            db_policy,
        }),
    ))
}

/// Creates a Wasmtime Engine configured with pooling allocator.
///
/// The pooling allocator pre-allocates memory for WASM instances, reducing
/// per-request instantiation overhead to ~5µs (vs ~50µs with on-demand).
fn create_engine(config: &PluginConfig) -> Result<Engine> {
    let mut wasmtime_config = Config::new();

    // Configure pooling allocator for efficient per-request instantiation
    let mut pooling_config = PoolingAllocationConfig::default();
    pooling_config.total_component_instances(config.max_instances);
    pooling_config.total_memories(config.max_instances);
    pooling_config.total_tables(config.max_instances);

    // Coherence between the two memory-bounding layers (WASM-4): the pooling
    // allocator's per-instance slab is the hard backstop and must never be
    // smaller than the per-`Store` limiter cap, or the allocator's opaque
    // `memory.grow` failure would fire *before* the limiter's clean, attributed
    // error. The limiter is the effective cap; when config sets it above the pool
    // slab we raise the slab to match (the limiter still wins as the reported
    // cap, the pool stays a coherent backstop ≥ it). Defaults coincide at 64 MiB.
    let pool_slab_bytes = config.max_memory_pages as usize * 65536;
    let slab_bytes = pool_slab_bytes.max(config.limits.max_memory_bytes);
    if slab_bytes > pool_slab_bytes {
        warn!(
            pool_slab_bytes,
            limiter_bytes = config.limits.max_memory_bytes,
            raised_slab_bytes = slab_bytes,
            "per-Store memory limiter exceeds the pooling-allocator slab; raising \
             the slab to keep the limiter the effective cap and the pool a backstop"
        );
    }
    pooling_config.max_memory_size(slab_bytes);

    wasmtime_config.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling_config));

    // Disable WASM threads — plugins run single-threaded and shared memory
    // is not needed. This also avoids RUSTSEC-2025-0118 (shared memory unsoundness).
    wasmtime_config.wasm_threads(false);

    // Enable epoch-based interruption to prevent infinite loops.
    // Plugins get a deadline set per invocation in the dispatcher.
    wasmtime_config.epoch_interruption(true);

    // Fuel metering (WASM-4, opt-in). Off by default: wasmtime only injects
    // per-operator fuel accounting into generated code when this flag is set, so
    // leaving it off imposes no codegen or runtime overhead. When enabled it
    // coexists with epoch interruption; each `Store` receives its budget in
    // `instantiate_and_call_export`.
    if config.limits.enable_fuel {
        wasmtime_config.consume_fuel(true);
    }

    // Optimize for speed
    wasmtime_config.cranelift_opt_level(wasmtime::OptLevel::Speed);

    Engine::new(&wasmtime_config)
        .into_anyhow()
        .context("failed to create wasmtime engine with pooling allocator")
}

/// Prefix all kernel host-interface imports share in the WASM import section.
///
/// A plugin imports host functions under the module string
/// `trovato:kernel/<interface>` (e.g. `trovato:kernel/logging`), matching the
/// host `func_wrap` module strings and the SDK `#[link(wasm_import_module = …)]`
/// blocks. So a compiled module's import section is an authoritative statement of
/// which host interfaces it needs.
const KERNEL_IMPORT_PREFIX: &str = "trovato:kernel/";

/// Build the per-plugin linker (WASM-1, deny-unless-declared).
///
/// Exposes only the host interfaces the plugin declares under
/// `[capabilities] host_interfaces` (`capabilities: None` and an empty list both
/// yield WASI stubs only — deny-all host imports), plus the baseline WASI stubs
/// every `wasm32-wasip1` module needs.
///
/// Before linking, this performs the load-time **import-vs-declaration pre-check**:
/// it walks the compiled module's imports, collects the distinct
/// `trovato:kernel/<iface>` interfaces it actually uses, and fails the load if any
/// is not declared — turning Wasmtime's raw mid-request "unknown import" trap into
/// a declarative startup error naming the plugin, the interface, and the fix.
///
/// # Errors
///
/// Returns an error if the module imports a `trovato:kernel/<iface>` interface it
/// does not declare, or if linker construction fails.
fn build_plugin_linker(
    engine: &Engine,
    module: &Module,
    info: &PluginInfo,
) -> Result<Linker<PluginState>> {
    let declared: Vec<String> = info
        .capabilities
        .as_ref()
        .map(|caps| caps.host_interfaces.clone())
        .unwrap_or_default();

    // Load-time import-vs-declaration pre-check. WASI imports
    // (`wasi_snapshot_preview1`) are baseline and excluded.
    let declared_set: HashSet<&str> = declared.iter().map(String::as_str).collect();
    let mut undeclared: Vec<&str> = module
        .imports()
        .filter_map(|imp| imp.module().strip_prefix(KERNEL_IMPORT_PREFIX))
        .filter(|iface| !declared_set.contains(iface))
        .collect();
    undeclared.sort_unstable();
    undeclared.dedup();
    if let Some(iface) = undeclared.first() {
        anyhow::bail!(
            "plugin '{}' imports host interface '{}' but does not declare \
             host_interfaces = [..., \"{}\"] in its [capabilities] manifest",
            info.name,
            iface,
            iface
        );
    }

    // Load-time DB-capability coherence pre-check (WASM-2 / D-19 §6). A manifest
    // that declares a DB-scoping field (`raw_sql = true` or a non-empty
    // `db_tables`) without granting the `db` host interface is incoherent: the
    // per-plugin linker will never link `db`, so the declaration can never take
    // effect. Surface it as a declarative startup error, mirroring the
    // unknown-import pre-check above, rather than letting it pass silently.
    if let Some(caps) = &info.capabilities {
        let declares_db = caps.host_interfaces.iter().any(|i| i == "db");
        if caps.raw_sql && !declares_db {
            anyhow::bail!(
                "plugin '{}' declares raw_sql = true but does not declare \
                 host_interfaces = [..., \"db\"] in its [capabilities] manifest",
                info.name
            );
        }
        if !caps.db_tables.is_empty() && !declares_db {
            anyhow::bail!(
                "plugin '{}' declares db_tables but does not declare \
                 host_interfaces = [..., \"db\"] in its [capabilities] manifest",
                info.name
            );
        }
    }

    let mut linker = Linker::new(engine);

    // WASI stubs are baseline — always linked, independent of host_interfaces.
    add_wasi_stubs(&mut linker)?;

    // Expose exactly the declared host-interface subset (deny-unless-declared).
    crate::host::register_declared(&mut linker, &declared)?;

    Ok(linker)
}

/// Add minimal WASI stubs for wasi_snapshot_preview1.
///
/// These stubs allow plugins compiled for wasm32-wasip1 to run
/// without full WASI support.
fn add_wasi_stubs(linker: &mut Linker<PluginState>) -> Result<()> {
    // fd_write(fd, iovs, iovs_len, nwritten) -> errno
    // Stub that returns ENOSYS (not supported)
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |_fd: i32, _iovs: i32, _iovs_len: i32, _nwritten: i32| -> i32 {
                52 // ENOSYS
            },
        )
        .into_anyhow()?;

    // random_get(buf, buf_len) -> errno
    // Fills buffer with cryptographically secure random bytes.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: wasmtime::Caller<'_, PluginState>, buf: i32, buf_len: i32| -> i32 {
                use rand::RngCore;

                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return 8; // EBADF
                };
                let data = memory.data_mut(&mut caller);
                let buf = buf as usize;
                let len = buf_len as usize;
                if buf + len > data.len() {
                    return 21; // EFAULT
                }
                rand::thread_rng().fill_bytes(&mut data[buf..buf + len]);
                0 // Success
            },
        )
        .into_anyhow()?;

    // environ_get(environ, environ_buf) -> errno
    // Stub that returns no environment variables
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_environ: i32, _environ_buf: i32| -> i32 {
                0 // Success (no env vars)
            },
        )
        .into_anyhow()?;

    // environ_sizes_get(environ_count, environ_buf_size) -> errno
    // Stub that returns 0 env vars
    linker
        .func_wrap(
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
        )
        .into_anyhow()?;

    // proc_exit(code) -> never returns
    // Stub that panics (shouldn't be called)
    linker
        .func_wrap("wasi_snapshot_preview1", "proc_exit", |_code: i32| {
            // Can't actually exit from a plugin
        })
        .into_anyhow()?;

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
            ..PluginConfig::default()
        };
        let runtime = PluginRuntime::new(&config);
        assert!(runtime.is_ok());
    }

    // --- Plugin search path ---

    fn scratch_plugins_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trovato_plugins_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch plugins root");
        dir
    }

    fn write_plugin(root: &Path, name: &str, version: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("create plugin dir");
        std::fs::write(
            dir.join(format!("{name}.info.toml")),
            format!(
                "name = \"{name}\"\n\
                 description = \"scratch\"\n\
                 version = \"{version}\"\n\
                 api_version = \"1.0\"\n\
                 default_enabled = false\n\
                 dependencies = []\n"
            ),
        )
        .expect("write info.toml");
    }

    /// Discovery spans every directory on the search path.
    ///
    /// This is what lets an application ship its plugins from its own
    /// repository instead of copying build artifacts into the kernel tree.
    #[tokio::test]
    async fn discovery_spans_the_whole_search_path() {
        let kernel = scratch_plugins_root("kernel");
        let app = scratch_plugins_root("app");
        write_plugin(&kernel, "core_thing", "1.0.0");
        write_plugin(&app, "app_thing", "2.0.0");

        let found = PluginRuntime::discover_plugins(&[kernel.clone(), app.clone()]).await;

        assert!(found.contains_key("core_thing"), "kernel plugin discovered");
        assert!(found.contains_key("app_thing"), "app plugin discovered");

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A later directory wins a name collision, so an app can shadow a plugin
    /// of the same name that appears earlier on the path.
    #[tokio::test]
    async fn later_directory_wins_a_name_collision() {
        let kernel = scratch_plugins_root("kernel_shadow");
        let app = scratch_plugins_root("app_shadow");
        write_plugin(&kernel, "shared", "1.0.0");
        write_plugin(&app, "shared", "9.9.9");

        let found = PluginRuntime::discover_plugins(&[kernel.clone(), app.clone()]).await;

        let (info, dir) = found
            .get("shared")
            .expect("collision resolves to one entry");
        assert_eq!(info.version, "9.9.9", "the later directory must win");
        assert!(dir.starts_with(&app), "and its path must be the later one");

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A single-directory search path behaves exactly as the old single-path
    /// argument did, which is what keeps existing deployments working.
    #[tokio::test]
    async fn single_directory_search_path_is_unchanged() {
        let only = scratch_plugins_root("only");
        write_plugin(&only, "solo", "1.2.3");

        let found = PluginRuntime::discover_plugins(std::slice::from_ref(&only)).await;
        assert_eq!(found.len(), 1);
        assert_eq!(found.get("solo").expect("present").0.version, "1.2.3");

        std::fs::remove_dir_all(&only).ok();
    }

    /// A directory that does not exist is skipped rather than failing the whole
    /// scan, so an optional app directory is safe to leave on the path.
    #[tokio::test]
    async fn absent_directory_on_the_path_is_skipped() {
        let real = scratch_plugins_root("real");
        write_plugin(&real, "present", "1.0.0");
        let absent = std::env::temp_dir().join("trovato_plugins_definitely_absent");
        std::fs::remove_dir_all(&absent).ok();

        let found = PluginRuntime::discover_plugins(&[absent, real.clone()]).await;
        assert!(found.contains_key("present"));

        std::fs::remove_dir_all(&real).ok();
    }
}
