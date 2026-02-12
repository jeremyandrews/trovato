//! Host infrastructure for Phase 0 benchmarks.
//!
//! Provides Wasmtime Engine with pooling allocator and stub host functions
//! that simulate the Kernel-side functions plugins call across the WASM boundary.

// Allow dead code for items that will be used in later stories (1.2-1.6)
#![allow(dead_code)]

use std::collections::HashMap;

use anyhow::{Context, Result};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Memory, Module, PoolingAllocationConfig,
    Store,
};

/// Configuration for the benchmark host environment.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Maximum number of concurrent instances (for pooling allocator).
    pub max_instances: u32,
    /// Maximum memory pages per instance (64KB per page).
    pub max_memory_pages: u64,
    /// Enable async support for async host functions.
    pub async_support: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            max_instances: 1000,
            max_memory_pages: 1024, // 64MB max per instance
            async_support: true,
        }
    }
}

/// Creates a Wasmtime Engine configured with pooling allocator.
///
/// The pooling allocator pre-allocates memory for WASM instances, reducing
/// per-request instantiation overhead to ~5µs (vs ~50µs with on-demand).
pub fn create_engine(config: &HostConfig) -> Result<Engine> {
    let mut wasmtime_config = Config::new();

    // Enable async support for async host functions (db queries, etc.)
    wasmtime_config.async_support(config.async_support);

    // Enable WASI (needed for wasm32-wasip1 target)
    // Note: For core WASI support, we use wasmtime-wasi crate integration

    // Configure pooling allocator for efficient per-request instantiation
    let mut pooling_config = PoolingAllocationConfig::default();
    pooling_config.total_component_instances(config.max_instances);
    pooling_config.total_memories(config.max_instances);
    pooling_config.total_tables(config.max_instances);
    pooling_config.max_memory_size(config.max_memory_pages as usize * 65536);

    wasmtime_config.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling_config));

    // Optimize for our use case
    wasmtime_config.cranelift_opt_level(wasmtime::OptLevel::Speed);

    Engine::new(&wasmtime_config).context("failed to create wasmtime engine with pooling allocator")
}

/// Simulated host state for a single request.
///
/// In the real Kernel, this would hold database connections, user session,
/// and request context. For benchmarks, it holds canned data.
pub struct StubHostState {
    /// Canned item data, keyed by handle.
    pub items: HashMap<i32, serde_json::Value>,
    /// Canned permission results.
    pub permissions: HashMap<String, bool>,
    /// Canned variables.
    pub variables: HashMap<String, String>,
    /// Log output buffer.
    pub log_buffer: Vec<String>,
    /// Request context (per-request key-value store).
    pub request_context: HashMap<String, String>,
    /// Current user ID.
    pub current_user_id: String,
}

impl Default for StubHostState {
    fn default() -> Self {
        Self::new()
    }
}

impl StubHostState {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
            permissions: HashMap::new(),
            variables: HashMap::new(),
            log_buffer: Vec::new(),
            request_context: HashMap::new(),
            current_user_id: "anonymous".to_string(),
        }
    }

    /// Load a fixture item at the given handle index.
    pub fn load_item(&mut self, handle: i32, item: serde_json::Value) {
        self.items.insert(handle, item);
    }

    /// Stub: get the title from the item at the given handle.
    pub fn get_title(&self, handle: i32) -> Option<String> {
        self.items
            .get(&handle)
            .and_then(|item| item.get("title"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Stub: get the item type.
    pub fn get_type(&self, handle: i32) -> Option<String> {
        self.items
            .get(&handle)
            .and_then(|item| item.get("type"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Stub: get the item ID.
    pub fn get_id(&self, handle: i32) -> Option<String> {
        self.items
            .get(&handle)
            .and_then(|item| item.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Stub: get a field value from the item at the given handle.
    pub fn get_field_string(&self, handle: i32, field_name: &str) -> Option<String> {
        self.items
            .get(&handle)
            .and_then(|item| item.get("fields"))
            .and_then(|fields| fields.get(field_name))
            .and_then(|field| {
                // Handle both {"value": "..."} and direct string
                field
                    .get("value")
                    .and_then(|v| v.as_str())
                    .or_else(|| field.as_str())
                    .map(|s| s.to_string())
            })
    }

    /// Stub: get an integer field value.
    pub fn get_field_int(&self, handle: i32, field_name: &str) -> Option<i64> {
        self.items
            .get(&handle)
            .and_then(|item| item.get("fields"))
            .and_then(|fields| fields.get(field_name))
            .and_then(|field| field.get("value").and_then(|v| v.as_i64()))
    }

    /// Stub: get a float field value.
    pub fn get_field_float(&self, handle: i32, field_name: &str) -> Option<f64> {
        self.items
            .get(&handle)
            .and_then(|item| item.get("fields"))
            .and_then(|fields| fields.get(field_name))
            .and_then(|field| field.get("value").and_then(|v| v.as_f64()))
    }

    /// Stub: get a field as raw JSON string.
    pub fn get_field_json(&self, handle: i32, field_name: &str) -> Option<String> {
        self.items
            .get(&handle)
            .and_then(|item| item.get("fields"))
            .and_then(|fields| fields.get(field_name))
            .map(|field| field.to_string())
    }

    /// Stub: set a field value on the item at the given handle.
    pub fn set_field_string(&mut self, handle: i32, field_name: &str, value: &str) {
        if let Some(item) = self.items.get_mut(&handle)
            && let Some(fields) = item.get_mut("fields")
        {
            fields[field_name] = serde_json::json!({ "value": value });
        }
    }

    /// Stub: set an integer field value.
    pub fn set_field_int(&mut self, handle: i32, field_name: &str, value: i64) {
        if let Some(item) = self.items.get_mut(&handle)
            && let Some(fields) = item.get_mut("fields")
        {
            fields[field_name] = serde_json::json!({ "value": value });
        }
    }

    /// Stub: set a title on the item.
    pub fn set_title(&mut self, handle: i32, value: &str) {
        if let Some(item) = self.items.get_mut(&handle) {
            item["title"] = serde_json::Value::String(value.to_string());
        }
    }

    /// Stub: check if the current user has a permission.
    pub fn user_has_permission(&self, permission: &str) -> bool {
        self.permissions.get(permission).copied().unwrap_or(false)
    }

    /// Stub: get a variable value.
    pub fn get_variable(&self, name: &str, default: &str) -> String {
        self.variables
            .get(name)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    /// Stub: set a variable value.
    pub fn set_variable(&mut self, name: &str, value: &str) {
        self.variables.insert(name.to_string(), value.to_string());
    }

    /// Stub: log a message.
    pub fn log_message(&mut self, level: &str, plugin: &str, message: &str) {
        self.log_buffer
            .push(format!("[{level}] {plugin}: {message}"));
    }

    /// Stub: simulate a database query (returns canned JSON).
    pub fn db_query(&self, _query_json: &str) -> Result<String, String> {
        Ok(serde_json::json!([]).to_string())
    }

    /// Stub: get request context value.
    pub fn get_request_context(&self, key: &str) -> Option<String> {
        self.request_context.get(key).cloned()
    }

    /// Stub: set request context value.
    pub fn set_request_context(&mut self, key: &str, value: &str) {
        self.request_context
            .insert(key.to_string(), value.to_string());
    }
}

/// Creates a Linker with basic host functions registered.
///
/// This registers the `logging` and `variables` interfaces from the WIT.
/// Additional interfaces (item-api, db, etc.) are added in later stories.
pub fn create_linker(engine: &Engine) -> Result<Linker<StubHostState>> {
    let mut linker = Linker::new(engine);

    // Register logging::log host function
    // WIT: log: func(level: string, plugin: string, message: string);
    linker.func_wrap(
        "trovato:kernel/logging",
        "log",
        |mut caller: wasmtime::Caller<'_, StubHostState>,
         level_ptr: i32,
         level_len: i32,
         plugin_ptr: i32,
         plugin_len: i32,
         message_ptr: i32,
         message_len: i32| {
            // Read strings from WASM memory
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(mem)) => mem,
                _ => return,
            };

            let level = read_string(&memory, &caller, level_ptr, level_len).unwrap_or_default();
            let plugin = read_string(&memory, &caller, plugin_ptr, plugin_len).unwrap_or_default();
            let message =
                read_string(&memory, &caller, message_ptr, message_len).unwrap_or_default();

            caller.data_mut().log_message(&level, &plugin, &message);
        },
    )?;

    // Register variables::get host function
    // WIT: get: func(name: string, default-value: string) -> string;
    // Note: For simplicity in Phase 0, we use a simpler ABI.
    // The actual implementation will use wit-bindgen generated bindings.
    linker.func_wrap(
        "trovato:kernel/variables",
        "get",
        |mut caller: wasmtime::Caller<'_, StubHostState>,
         name_ptr: i32,
         name_len: i32,
         default_ptr: i32,
         default_len: i32|
         -> i64 {
            let memory = match caller.get_export("memory") {
                Some(wasmtime::Extern::Memory(mem)) => mem,
                _ => return 0,
            };

            let name = read_string(&memory, &caller, name_ptr, name_len).unwrap_or_default();
            let default =
                read_string(&memory, &caller, default_ptr, default_len).unwrap_or_default();

            let result = caller.data().get_variable(&name, &default);

            // Return pointer and length packed into i64 (simplified for benchmarks)
            // Real implementation will use proper canonical ABI
            result.len() as i64
        },
    )?;

    Ok(linker)
}

/// Helper to read a string from WASM linear memory.
fn read_string<T>(
    memory: &Memory,
    caller: &wasmtime::Caller<'_, T>,
    ptr: i32,
    len: i32,
) -> Option<String> {
    if len <= 0 {
        return Some(String::new());
    }

    let data = memory.data(caller);
    let start = ptr as usize;
    let end = start + len as usize;

    if end > data.len() {
        return None;
    }

    String::from_utf8(data[start..end].to_vec()).ok()
}

/// Compiles a WASM module from bytes.
pub fn compile_module(engine: &Engine, wasm_bytes: &[u8]) -> Result<Module> {
    Module::new(engine, wasm_bytes).context("failed to compile WASM module")
}

/// Compiles a WASM module from a file path.
pub fn compile_module_from_file(engine: &Engine, path: &std::path::Path) -> Result<Module> {
    Module::from_file(engine, path)
        .with_context(|| format!("failed to compile WASM module from {}", path.display()))
}

/// Creates a new Store with fresh host state.
pub fn create_store(engine: &Engine) -> Store<StubHostState> {
    Store::new(engine, StubHostState::new())
}

/// Creates a new Store with the provided host state.
pub fn create_store_with_state(engine: &Engine, state: StubHostState) -> Store<StubHostState> {
    Store::new(engine, state)
}

/// Benchmark host environment holding the engine and linker.
pub struct BenchHost {
    pub engine: Engine,
    pub linker: Linker<StubHostState>,
}

impl BenchHost {
    /// Create a new benchmark host with default configuration.
    pub fn new() -> Result<Self> {
        Self::with_config(&HostConfig::default())
    }

    /// Create a new benchmark host with custom configuration.
    pub fn with_config(config: &HostConfig) -> Result<Self> {
        let engine = create_engine(config)?;
        let linker = create_linker(&engine)?;
        Ok(Self { engine, linker })
    }

    /// Compile a WASM module.
    pub fn compile(&self, wasm_bytes: &[u8]) -> Result<Module> {
        compile_module(&self.engine, wasm_bytes)
    }

    /// Compile a WASM module from a file.
    pub fn compile_from_file(&self, path: &std::path::Path) -> Result<Module> {
        compile_module_from_file(&self.engine, path)
    }

    /// Create a new store for a request.
    pub fn create_store(&self) -> Store<StubHostState> {
        create_store(&self.engine)
    }

    /// Create a new store with pre-populated state.
    pub fn create_store_with_state(&self, state: StubHostState) -> Store<StubHostState> {
        create_store_with_state(&self.engine, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_creates_with_pooling_allocator() {
        let config = HostConfig::default();
        let engine = create_engine(&config);
        assert!(engine.is_ok(), "Engine should create successfully");
    }

    #[test]
    fn linker_registers_host_functions() {
        let config = HostConfig::default();
        let engine = create_engine(&config).unwrap();
        let linker = create_linker(&engine);
        assert!(linker.is_ok(), "Linker should create with host functions");
    }

    #[test]
    fn bench_host_initializes() {
        let host = BenchHost::new();
        assert!(host.is_ok(), "BenchHost should initialize successfully");
    }

    #[test]
    fn stub_host_state_operations() {
        let mut state = StubHostState::new();

        // Test logging
        state.log_message("info", "test", "hello world");
        assert_eq!(state.log_buffer.len(), 1);
        assert_eq!(state.log_buffer[0], "[info] test: hello world");

        // Test variables
        assert_eq!(state.get_variable("missing", "default"), "default");
        state.set_variable("site_name", "Trovato");
        assert_eq!(state.get_variable("site_name", "default"), "Trovato");

        // Test permissions
        assert!(!state.user_has_permission("admin"));
        state.permissions.insert("admin".to_string(), true);
        assert!(state.user_has_permission("admin"));
    }

    #[test]
    fn stub_host_state_item_operations() {
        let mut state = StubHostState::new();

        let item = serde_json::json!({
            "id": "test-123",
            "type": "blog",
            "title": "Test Post",
            "fields": {
                "field_body": { "value": "Hello world" },
                "field_rating": { "value": 5 }
            }
        });

        state.load_item(0, item);

        assert_eq!(state.get_title(0), Some("Test Post".to_string()));
        assert_eq!(state.get_type(0), Some("blog".to_string()));
        assert_eq!(state.get_id(0), Some("test-123".to_string()));
        assert_eq!(
            state.get_field_string(0, "field_body"),
            Some("Hello world".to_string())
        );
        assert_eq!(state.get_field_int(0, "field_rating"), Some(5));

        // Test modification
        state.set_field_string(0, "field_body", "Updated content");
        assert_eq!(
            state.get_field_string(0, "field_body"),
            Some("Updated content".to_string())
        );
    }
}
