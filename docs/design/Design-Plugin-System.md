# Trovato Design: Plugin & Tap System

*Sections 3-4 of the v2.1 Design Document*

---

## 3. The Plugin System (WASM)

This is the hardest part of the whole project and the core of the architecture.

### Why WASM

The requirement is that you can enable/disable plugins without recompiling the Kernel. In Rust, the options are:

1. **Dynamic linking (dylib)** — Rust has no stable ABI. A plugin compiled with one Rust version may not work with another. Not viable.
2. **Scripting language (Lua, Rhai)** — Works, but plugins would need to be written in a different language. Loses Rust's type system.
3. **WebAssembly** — Plugins are compiled to `.wasm`, loaded at runtime, executed in a sandbox. Can be written in Rust or any language that compiles to WASM. Interface is well-defined via WIT.

WASM is the only option that gives us runtime loading, language flexibility, a security sandbox, and near-native performance.

### The Concurrency Problem & Solution

Wasmtime's `Store` is `!Send` and `!Sync`. We cannot share a Store across threads. A naive design that holds mutable `Store<PluginState>` per plugin in the `PluginRegistry` means only one request can invoke taps at a time. For a web server handling concurrent requests, this is a showstopper.

**The Solution: Pooled Instantiation Model.** We compile plugins once (the `Plugin` struct is `Send+Sync`). For *each HTTP request*, we grab a fresh `Store` from a pool, instantiate the plugin, run the taps, and drop the Store at the end of the request. The pooling allocator makes instantiation cheap (~5µs).

### The Request State

This struct is passed to every tap invocation, ensuring isolation. It lazily instantiates only the plugins needed for that request.

```rust
pub struct RequestState {
    pub db: PgPool,
    pub redis: redis::Client,
    pub current_user_id: Uuid,
    pub stage_id: Option<String>,
    // Shared memory for plugins within this request (replaces global variables)
    pub context: Arc<tokio::sync::Mutex<RequestContext>>,
    // Live instances for this request
    stores: tokio::sync::Mutex<HashMap<String, (Store<PluginState>, Instance)>>,
}

impl RequestState {
    // Lazily instantiate plugins only when needed
    pub async fn get_or_create_store(
        &self, engine: &Engine, compiled: &CompiledPlugin,
    ) -> Result<&mut (Store<PluginState>, Instance), PluginError> {
        let mut stores = self.stores.lock().await;
        if !stores.contains_key(&compiled.name) {
            // Pool allocator makes this cheap (~5µs)
            let mut store = Store::new(engine, PluginState::new(
                self.db.clone(), self.context.clone(),
            ));
            let mut linker = Linker::new(engine);
            link_host_functions(&mut linker)?;

            let instance = linker.instantiate_async(
                &mut store, &compiled.plugin,
            ).await?;
            stores.insert(compiled.name.clone(), (store, instance));
        }
        Ok(stores.get_mut(&compiled.name).unwrap())
    }
}
```

### The WASM Proof-of-Concept (Phase 0)

The entire architecture depends on WASM working at acceptable latency. This is the single highest-risk component. Before writing any other code, spend the first two weeks proving or disproving this.

Phase 0 has three critical objectives:

1. **Benchmark handle-based vs. full-serialization data access.** Under identical conditions (500 calls, 3 fields read + 1 field written per call), measure wall-clock time and p50/p95/p99 latency for both modes. If handle-based achieves >5x speedup (expected), it becomes the default WIT pattern. If full-serialization is acceptable (<1ms p95 for a 4KB payload), the simpler API may be preferred.

2. **Benchmark Store pooling under concurrency.** 100 parallel requests, each instantiating a plugin, calling a tap, and returning. Measure p50/p95/p99 instantiation and execution latency per request. This validates that the pooled Store model scales.

3. **Validate async host functions.** WASM → host function → SQLx query → return. Confirm no deadlocks under the Tokio runtime and measure latency.

#### What to Build

A standalone Rust binary (not the full Kernel) that loads WASM plugins and exercises both data access modes under identical conditions. No database, no HTTP, no Redis. Just the WASM runtime, the RequestState, and the pooling allocator.

#### The Host Binary

1. Initialize Wasmtime with the pooling allocator enabled. Use the same `Engine` configuration planned for production.
2. Load a test plugin compiled from a separate Rust crate targeting `wasm32-wasip1`.
3. Register four host functions: `db_query` (returns canned JSON), `user_has_permission` (returns bool), `log_message` (prints to stderr), and `get_variable` (returns a string from a HashMap).
4. Call the plugin's `tap_item_view` export with a realistic Item payload: a JSON object with 15 fields, including nested arrays and record references, totaling roughly 4KB serialized.
5. Read back the modified payload and verify the plugin changed a specific field value.
6. Repeat 500 times in a loop and measure wall-clock time. This simulates a Gather page rendering 50 items with 10 plugins each.
7. **Benchmark `Store` pooling with high concurrency** (100 parallel requests).
8. **Validate async host functions** (WASM → Rust → SQLx bridge).
9. **Measure serialization overhead** (passing 50KB JSON objects back and forth).

#### The Test Plugins

Two test plugins (or one plugin with two modes):

**Handle-based test:** Receives an item handle, calls `get-title` and `get-field-string("field_body")`, calls `user_has_permission` with a hardcoded permission string, if granted calls `set-field-string` to add a computed display title, and returns a JSON Render Element.

**Full-serialization test:** Receives the full Item JSON payload (same 4KB item, 15 fields), parses it with `serde_json`, calls `user_has_permission`, if granted adds a computed field, and returns the modified JSON.

#### Success Criteria

The spike produces a written recommendation on data access mode with benchmark numbers:

- **Handle-based:** 500 calls measuring read (3 fields) + write (1 field) + return render element latency.
- **Full-serialization:** 500 calls with identical 4KB payloads.
- **Hybrid:** 500 calls where the plugin receives a handle, reads fields, but returns full JSON render output (bridges both modes).

If handle-based achieves >5x speedup or full-serialization remains <1ms p95, the spike passes. This recommendation rewrites the data access section of the design if needed.

Store pooling and async host functions should complete their 100-parallel and SQLx bridge benchmarks without deadlock and with latency <10ms p95 per request.

If all three objectives pass, no code is written yet. If any fails, evaluate Extism's higher-level abstractions. If that fails, fall back to an embedded scripting language (Rhai or Lua via mlua).

### The Boundary Problem and Solution

Every call across the WASM boundary requires serialization. When the Kernel naively passes an Item to a plugin's `tap_item_view`, it must serialize the Item to bytes, write those bytes into the WASM plugin's linear memory, call the plugin's exported function with a pointer and length, read the response back out, and deserialize it.

For a single call, this is fast (microseconds). For `tap_item_view` called on 50 items by 10 plugins, that's 500 serialize/deserialize cycles. At 0.5ms per cycle, that's 250ms of latency per page load.

**The Solution: Handle-Based Access (Primary).** Instead of serializing the entire Item, the Kernel passes an opaque handle (an integer). The plugin calls host functions like `get-field-string(handle, "title")` to read fields on demand. The Kernel looks up the handle in the per-request `RequestState`, fetches the field value from memory (no serialization), and returns it. Only the specific field value crosses the boundary, and only if the plugin asks for it.

This is the default pattern, declared in `.info.toml` as `data_mode = "handle"`. For the common case — read a few fields, maybe modify one, return a render element — this eliminates serialization entirely.

**Full Serialization (Fallback).** For plugins that genuinely need bulk mutation or restructuring, the old pattern remains: pass the full JSON, get the full JSON back. These plugins declare `data_mode = "full"` and accept the cost.

**Other Mitigation Strategies.** Batch taps (`tap_item_view_multiple` with the full list) are also possible but less fine-grained. Selective host functions (e.g., `user_has_permission(uid, permission) -> bool`) remain useful for any repeated operation that would otherwise cross the boundary multiple times.

### Host Functions: Async/Sync Bridge

WASM is synchronous; `sqlx` is async. Calling `tile_on` inside a Tokio runtime to bridge sync WASM calls to async DB calls will deadlock unless you use `spawn_blocking` or a separate thread pool. We bridge this using Wasmtime's async support: the WASM guest sees a blocking call; the host suspends the WASM stack and awaits the Future.

### Plugin Structure

A plugin is a separate Rust crate compiled to `wasm32-wasip1`:

```
plugins/
  blog/
    blog.info.toml      # metadata
    blog.wasm            # compiled plugin
```

The `info.toml` file (replaces Drupal's `.info` files):

```toml
name = "blog"
description = "Provides a blog content type"
version = "1.0.0"
dependencies = ["item"]

[taps]
implements = [
    "tap_item_info",
    "tap_item_view",
    "tap_menu",
    "tap_perm",
]

# Optional: Declare data access mode per tap (default is "handle")
[taps.options]
tap_item_view = { data_mode = "handle" }  # or "full" for bulk mutation
```

The `data_mode` option controls how the plugin receives data:
- `"handle"` (default): Plugin receives an integer handle, calls host functions to read/write fields.
- `"full"`: Plugin receives the complete JSON payload, returns modified JSON.

### The WIT Interface (v2): Handle-Based Data Access

WIT (WebAssembly Interface Types) defines the contract between the Kernel and plugins. The primary pattern is handle-based data access: the Kernel passes opaque handles (integer IDs) to the plugin, and the plugin calls host functions to read or write individual fields. This eliminates bulk serialization overhead for the common case.

**Handle-Based Mode (default).** When a plugin declares `data_mode = "handle"` in its `.info.toml`, the Kernel passes an item handle instead of full JSON:

```wit
interface item-api {
    // Read
    get-title: func(item-handle: s32) -> string;
    get-field-string: func(item-handle: s32, field-name: string) -> option<string>;
    get-field-int: func(item-handle: s32, field-name: string) -> option<s64>;
    get-field-json: func(item-handle: s32, field-name: string) -> option<string>;
    get-type: func(item-handle: s32) -> string;
    get-author-id: func(item-handle: s32) -> string;

    // Write
    set-field-string: func(item-handle: s32, field-name: string, value: string);
    set-field-int: func(item-handle: s32, field-name: string, value: s64);
    set-field-json: func(item-handle: s32, field-name: string, value-json: string);
}
```

The Kernel holds the actual Item in the `RequestState`. The handle is an index into a per-request `Vec<Item>`. No serialization unless the plugin asks for a specific field value.

**Full Serialization Mode (opt-in).** For plugins that need to restructure the entire item (e.g., migration, bulk transform), the full JSON pattern remains available. Plugins declare this via:

```toml
[taps]
implements = ["tap_item_view"]

[taps.options]
tap_item_view = { data_mode = "full" }
```

**Rationale.** A Gather query rendering 50 items with 10 plugins implementing `tap_item_view` produces 500 calls. Under full serialization at 0.5ms per round-trip (optimistic for a 4KB item), that's 250ms of pure serialization latency per page load. Handle-based access eliminates this for the common case (read a few fields, possibly modify one, return render output). Plugins that genuinely need bulk mutations opt into full serialization and accept the cost.

The authoritative WIT interface (with all host functions and both handle-based/full-serialization exports) is in the [[Projects/Trovato/Design-Plugin-SDK|Plugin SDK Spec]], Section 6.

### Export Routing: Handle-Based vs Full-Serialization

The WIT world defines separate exports for each mode:

```wit
// Handle-based (default)
export tap-item-view: func(item-handle: s32) -> string;

// Full-serialization (opt-in)
export tap-item-view-full: func(item-json: string) -> string;
```

A plugin only exports ONE variant — whichever its code uses. The routing works as follows:

1. **At load time:** The Kernel reads `data_mode` from `.info.toml`'s `[taps.options]` section and records it per plugin per tap.
2. **At compile time (plugin side):** The `#[plugin_tap]` macro determines which export to generate based on the function's parameter type: `&ItemHandle` → generates `tap-item-view` (handle-based); `&Item` → generates `tap-item-view-full` (full-serialization).
3. **At dispatch time:** The Kernel checks the recorded `data_mode` and calls the corresponding export name.
4. **On mismatch:** If `.info.toml` says `"handle"` but the plugin doesn't export `tap-item-view`, wasmtime returns a `MissingExport` error at first invocation. This is caught as a `PluginError` and logged. The fix is to align `.info.toml` with the function signature.

### Store Reuse Within a Request

The `RequestState` keys stores by plugin name (see `get_or_create_store` above). This means:

- **Within a single request**, if the same plugin handles two different taps (e.g., `tap_item_view` and `tap_menu`), the same `Store` and `Instance` are reused. The plugin's WASM linear memory persists across tap calls within that request. Plugins can use global variables for per-request state — similar to Drupal's `static` function variables.
- **Between requests**, Stores are dropped and returned to the pool. No state carries over. Every request starts clean.
- **Between plugins**, Stores are separate. Plugin A cannot access Plugin B's WASM memory. Communication between plugins uses host functions (`invoke_plugin`) or shared database state.

### Stage Context in Tap Invocations

Every tap invocation runs within a stage context. The `stage_id` in `RequestState` determines which version of content the tap sees. When a tap calls `item_load` via a host function, the Kernel automatically applies stage overrides — the plugin doesn't need to know about stages.

This means plugins are stage-transparent by default. A `tap_item_view` implementation that reads fields from an `ItemHandle` sees the stage version of those fields automatically. Plugins only need explicit stage awareness if they bypass the standard loading functions (e.g., using raw SQL queries).

### The Plugin Loader

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use wasmtime::*;

pub struct CompiledPlugin {
    pub name: String,
    pub plugin: Plugin,       // Send + Sync — safe to share
    pub taps: Vec<String>,
}

pub struct PluginRegistry {
    engine: Engine,
    plugins: HashMap<String, CompiledPlugin>,
    load_order: Vec<String>,
}

impl PluginRegistry {
    pub async fn load_all(
        engine: &Engine, plugins_dir: &PathBuf, db: &PgPool,
    ) -> Result<Self, PluginError> {
        let mut registry = Self {
            engine: engine.clone(),
            plugins: HashMap::new(),
            load_order: Vec::new(),
        };

        // 1. Discover plugins and read metadata
        let mut plugin_infos: HashMap<String, PluginInfo> = HashMap::new();
        let mut entries = tokio::fs::read_dir(plugins_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let info_path = entry.path().join(
                    format!("{}.info.toml",
                        entry.file_name().to_string_lossy())
                );
                if info_path.exists() {
                    let toml_str =
                        tokio::fs::read_to_string(&info_path).await?;
                    let info: PluginInfo = toml::from_str(&toml_str)?;
                    plugin_infos.insert(info.name.clone(), info);
                }
            }
        }

        // 2. Check which plugins are enabled
        let enabled: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM system WHERE type = 'plugin' AND status = 1"
        ).fetch_all(db).await?;

        // 3. Topological sort by dependencies
        let load_order = topological_sort(&plugin_infos, &enabled)?;

        // 4. Compile each plugin (but don't instantiate — that happens per-request)
        for plugin_name in &load_order {
            let info = &plugin_infos[plugin_name];
            let wasm_path = plugins_dir
                .join(&info.name)
                .join(format!("{}.wasm", info.name));

            let plugin = Plugin::from_file(&engine, &wasm_path)?;

            registry.plugins.insert(
                plugin_name.clone(),
                CompiledPlugin {
                    name: plugin_name.clone(),
                    plugin,
                    taps: info.taps.implements.clone(),
                },
            );
        }

        registry.load_order = load_order;
        Ok(registry)
    }

    pub fn get_implementors(&self, tap_name: &str) -> Vec<&CompiledPlugin> {
        self.load_order.iter()
            .filter_map(|name| self.plugins.get(name))
            .filter(|m| m.taps.contains(&tap_name.to_string()))
            .collect()
    }

    pub fn disable(&mut self, plugin_name: &str) {
        self.plugins.remove(plugin_name);
        self.load_order.retain(|n| n != plugin_name);
    }
}
```

### WASM Toolchain

**Decision: Core modules + wit-bindgen.** Plugins compile to `wasm32-wasip1` (core modules, WASI Preview 1). The WIT file defines the interface contract. `wit-bindgen` generates Rust bindings on both sides:

- **Plugin side:** `wit-bindgen` generates import stubs for host functions (item-api, db, cache, etc.) and export stubs that the `#[plugin_tap]` macro wraps around the developer's code.
- **Kernel side:** `wit-bindgen` generates host function bindings that the Kernel implements, plus call stubs for invoking plugin exports.

Memory management (alloc/dealloc, pointer/length passing) is handled by `wit-bindgen`'s generated code. Plugin authors never touch raw pointers. Kernel authors don't write `write_to_wasm`/`read_from_wasm` manually — `wit-bindgen` generates type-safe wrappers.

**Why not the full Component Model?** WASI Preview 2 and the component model canonical ABI are newer, and wasmtime's async support (critical for our SQLx bridge) is better tested with core modules. We use WIT as the interface definition regardless — migration to the component model in year two only changes the compilation target and runtime config, not the WIT file or plugin source code.

**Fallback:** If Phase 0 reveals that raw wasmtime + wit-bindgen is too much plumbing, Extism (a higher-level WASM host SDK) can be adopted mid-Phase-2 without changing the WIT file or plugin source code. Extism wraps wasmtime and handles memory/pooling automatically.

### WASM Memory Management (Reference)

This section shows what `wit-bindgen` generates under the hood, for debugging and understanding. You don't write this code yourself.

```rust
// Generated by wit-bindgen for the Kernel side.
// The plugin exports alloc/dealloc; wit-bindgen calls them automatically.
fn write_to_wasm(
    store: &mut Store<PluginState>, instance: &Instance,
    data: &str,
) -> Result<(u32, u32), PluginError> {
    let alloc = instance
        .get_typed_func::<u32, u32>(&mut *store, "alloc")
        .map_err(|_| PluginError::MissingExport("alloc"))?;
    let len = data.len() as u32;
    let ptr = alloc.call(&mut *store, len)?;
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or(PluginError::MissingMemory)?;
    memory.write(&mut *store, ptr as usize, data.as_bytes())?;
    Ok((ptr, len))
}

fn read_from_wasm(
    store: &mut Store<PluginState>, instance: &Instance,
    ptr: u32, len: u32,
) -> Result<String, PluginError> {
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or(PluginError::MissingMemory)?;
    let mut buf = vec![0u8; len as usize];
    memory.read(&store, ptr as usize, &mut buf)?;
    String::from_utf8(buf).map_err(|_| PluginError::InvalidUtf8)
}
```

### Plugin Dependencies: Topological Sort

```rust
use std::collections::{HashMap, HashSet};

pub fn topological_sort(
    plugins: &HashMap<String, PluginInfo>, enabled: &[String],
) -> Result<Vec<String>, PluginError> {
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();

    for name in enabled {
        if !visited.contains(name) {
            visit(name, plugins, &mut visited,
                  &mut visiting, &mut result)?;
        }
    }
    Ok(result)
}

fn visit(
    name: &str, plugins: &HashMap<String, PluginInfo>,
    visited: &mut HashSet<String>,
    visiting: &mut HashSet<String>,
    result: &mut Vec<String>,
) -> Result<(), PluginError> {
    if visiting.contains(name) {
        return Err(PluginError::CircularDependency(
            name.to_string()));
    }
    if visited.contains(name) { return Ok(()); }

    visiting.insert(name.to_string());
    let info = plugins.get(name)
        .ok_or_else(|| PluginError::MissingDependency(
            name.to_string()))?;
    for dep in &info.dependencies {
        visit(dep, plugins, visited, visiting, result)?;
    }
    visiting.remove(name);
    visited.insert(name.to_string());
    result.push(name.to_string());
    Ok(())
}
```

---

## 4. The Tap System

### How Drupal 6 Taps Worked

In Drupal 6, taps were "magic functions." If you named a function `blog_node_view()` in your `blog.plugin` file, Drupal would call it when any item was viewed. The discovery mechanism was literally `function_exists()` at runtime.

We can't do that in Rust/WASM. Instead, plugins declare their taps in `.info.toml`, and the Kernel builds a registry at startup.

### The Tap Registry

```rust
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct TapRegistration {
    pub plugin: String,
    pub weight: i32,
}

pub struct TapRegistry {
    taps: HashMap<String, Vec<TapRegistration>>,
}

impl TapRegistry {
    pub fn new() -> Self {
        Self { taps: HashMap::new() }
    }

    pub fn register(
        &mut self, tap_name: &str, plugin: &str, weight: i32,
    ) {
        let entry = self.taps
            .entry(tap_name.to_string()).or_default();
        entry.push(TapRegistration {
            plugin: plugin.to_string(), weight,
        });
        entry.sort_by_key(|r| r.weight);
    }

    pub fn get_implementors(
        &self, tap_name: &str,
    ) -> &[TapRegistration] {
        self.taps.get(tap_name)
            .map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn rebuild(
        &mut self, plugins: &HashMap<String, PluginInfo>,
    ) {
        self.taps.clear();
        for (name, info) in plugins {
            if !info.enabled { continue; }
            for tap in &info.taps.implements {
                self.register(tap, name, info.weight);
            }
        }
    }
}
```

---

## 5. Inter-Plugin Communication

### The Mechanism

Plugins can invoke functions in other plugins directly via the `invoke_plugin` host function. This enables edge cases where synchronous plugin-to-plugin calls are necessary.

### WIT Interface

```wit
interface plugin-api {
    invoke: func(plugin-name: string, function-name: string, payload: string)
        -> result<string, string>;
    plugin-exists: func(plugin-name: string) -> bool;
}
```

**`invoke(plugin_name, function_name, payload) -> Result<String, String>`**

Invokes an exported function in another plugin within the same request context. The payload is an arbitrary JSON string passed to the called plugin's function. Returns the function result as a JSON string, or an error.

**`plugin_exists(plugin_name) -> bool`**

Checks if a plugin is installed and enabled. Useful for graceful degradation when an optional plugin is not available.

### Implementation Details

- **Routing:** The Kernel's `invoke_plugin` host function retrieves the target plugin from the plugin registry. If a Store for that plugin is not yet instantiated in the per-request pool, it instantiates one. The target function is then called with the provided payload.
- **Context Sharing:** Both caller and callee execute within the same `RequestState`, ensuring shared access to the database, Redis, user context, and request-scoped storage.
- **Error Handling:** If the plugin does not exist or does not export the requested function, an error is returned. Plugin errors are also propagated back to the caller.

### Guidelines for Use

Most inter-plugin communication happens through taps and shared database state, both of which are already supported:

- Use **taps** for event-driven hooks (item creation, rendering, menu customization).
- Use **shared database state** for inter-plugin data exchange and coordination.
- Use **invoke_plugin** only for exceptional cases where direct synchronous calls are necessary and cannot be expressed via existing mechanisms.

### Phase Implementation

`invoke_plugin` is implemented in Phase 4 alongside the Gather Query Engine and Categories module.

---

