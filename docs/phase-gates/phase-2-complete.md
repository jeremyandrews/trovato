# Phase 2: Plugin Development Platform - Complete

**Date**: 2026-02-12
**Status**: All 18 stories complete
**Recommendation**: Proceed to Phase 3 (Content System)

## Executive Summary

Phase 2 implemented the complete WASM plugin system, validating the architecture decisions from Phase 0. The plugin runtime uses Wasmtime's pooling allocator for ~5µs instantiation, full-serialization for data passing (as proven optimal in Phase 0), and provides 7 host function modules for plugin-kernel communication.

Key deliverables:
- Plugin loader with .info.toml manifest parsing
- Tap registry with weight-based dispatch ordering
- `#[plugin_tap]` proc macro for ergonomic plugin development
- Reference blog plugin with 4 working tap exports
- 43 unit tests + 24 integration tests

## Story Completion

### Phase 2.1: Foundation

| Story | Description | Status |
|-------|-------------|--------|
| 3.1 | Update SDK types for full-serialization | ✅ |
| 3.3 | Simplify WIT interface | ✅ |
| 3.4 | Create .info.toml parser | ✅ |

### Phase 2.2: Core Runtime

| Story | Description | Status |
|-------|-------------|--------|
| 3.5 | Create WASM plugin loader | ✅ |
| 3.7 | Create tap registry | ✅ |
| 3.8 | Create RequestState | ✅ |

### Phase 2.3: Host Functions

| Story | Description | Status |
|-------|-------------|--------|
| 3.9 | Item host functions | ✅ |
| 3.10 | Database host functions | ✅ |
| 3.11 | User host functions | ✅ |
| 3.12 | Cache host functions | ✅ |
| 3.13 | Variables host functions | ✅ |
| 3.14 | Logging host functions | ✅ |

### Phase 2.4: Integration

| Story | Description | Status |
|-------|-------------|--------|
| 3.6 | Plugin dependency resolution | ✅ |
| 3.15 | Create tap dispatcher | ✅ |
| 3.16 | Create menu registry | ✅ |

### Phase 2.5: Proc Macro & Polish

| Story | Description | Status |
|-------|-------------|--------|
| 3.2 | Create #[plugin_tap] proc macro | ✅ |
| 3.17 | Update blog plugin | ✅ |
| 3.18 | Improve error messages | ✅ |

---

## Architecture

### File Structure

```
crates/kernel/src/
  plugin/
    mod.rs              # Module exports
    info_parser.rs      # .info.toml manifest parsing
    runtime.rs          # PluginRuntime, pooling allocator
    dependency.rs       # Topological sort, cycle detection
    error.rs            # PluginError types
  tap/
    mod.rs              # Module exports
    registry.rs         # TapRegistry, weight ordering
    dispatcher.rs       # TapDispatcher, async invocation
    request_state.rs    # RequestState, UserContext
  host/
    mod.rs              # register_all(), memory helpers
    item.rs             # get_item, save_item, delete_item, query_items
    db.rs               # select, insert, update, delete, query_raw
    user.rs             # current_user_id, current_user_has_permission
    cache.rs            # get, set, invalidate_tag
    variables.rs        # get, set
    request_context.rs  # get, set (per-request)
    logging.rs          # log
  menu/
    mod.rs              # Module exports
    registry.rs         # MenuRegistry, path matching

crates/plugin-sdk/
  src/
    lib.rs              # Re-exports macros
    types.rs            # Item, ContentTypeDefinition, etc.
    render.rs           # RenderElement

crates/plugin-sdk-macros/
  src/
    lib.rs              # #[plugin_tap], #[plugin_tap_result]

plugins/blog/
  src/lib.rs            # tap_item_info, tap_perm, tap_menu, tap_item_access
  blog.info.toml        # Plugin manifest
  blog.wasm             # Compiled WASM (227KB)
```

### Plugin Lifecycle

```
1. Startup
   ├── Create PluginRuntime (pooling allocator)
   ├── load_all(plugins_dir)
   │   ├── Parse .info.toml
   │   ├── Validate tap names
   │   ├── Compile WASM module
   │   └── Store in plugins HashMap
   ├── Build TapRegistry (index taps by name)
   └── Build MenuRegistry (collect from tap_menu)

2. Request
   ├── Create RequestState (user, services)
   ├── dispatcher.dispatch("tap_name", input_json, state)
   │   ├── Get handlers from registry (weight-ordered)
   │   ├── For each handler:
   │   │   ├── Create Store<RequestState>
   │   │   ├── instantiate_async (pooling: ~5µs)
   │   │   ├── Write input to WASM memory
   │   │   ├── call_async tap function
   │   │   └── Read output from WASM memory
   │   └── Collect results
   └── Process results
```

### Proc Macro Example

```rust
// Plugin code
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![ContentTypeDefinition {
        machine_name: "blog".into(),
        label: "Blog Post".into(),
        // ...
    }]
}

// Generated code
fn __inner_tap_item_info() -> Vec<ContentTypeDefinition> { /* original body */ }

#[unsafe(no_mangle)]
pub extern "C" fn tap_item_info(ptr: i32, len: i32) -> i64 {
    let result = __inner_tap_item_info();
    let output = serde_json::to_string(&result).unwrap();
    // Write to buffer, return ptr<<32|len
}
```

---

## Test Coverage

### Unit Tests (43 total)

| Module | Tests |
|--------|-------|
| plugin::info_parser | 5 |
| plugin::dependency | 7 |
| plugin::runtime | 2 |
| plugin::error | 3 |
| tap::registry | 4 |
| tap::dispatcher | 2 |
| tap::request_state | 5 |
| host::* | 7 |
| menu::registry | 7 |

### Integration Tests (24 total)

| Category | Tests |
|----------|-------|
| Plugin runtime | 4 |
| Plugin loading | 5 |
| Error handling | 3 |
| Tap registry | 3 |
| RequestState | 3 |
| Host functions | 2 |
| Menu registry | 2 |
| Dependency resolution | 3 |

---

## Verification

```bash
# All tests pass
cargo test -p trovato-kernel --lib
# running 43 tests ... ok

cargo test --test plugin_test
# running 24 tests ... ok

# Blog plugin builds to WASM
cargo build -p blog --target wasm32-wasip1 --release
# blog.wasm: 227KB

# Plugin loads successfully
cargo test --test plugin_test load_single_plugin
# ok
```

---

## Known Limitations

### Host Functions
Host functions are implemented with stub behavior for:
- `item.rs` - Returns stub data, not connected to database
- `db.rs` - Returns empty results
- `cache.rs` - No-op (no cache backend)
- `variables.rs` - Returns defaults, not persisted

These will be connected to real services in Phase 3.

### Async Support
The tap dispatcher uses `call_async` but host functions use `func_wrap` (synchronous). Full async host functions (`func_wrap_async`) will be needed when connecting to database.

### Error Recovery
Plugin panics are caught but WASM instance may be corrupted. Current behavior logs and continues to next plugin. Consider instance recycling for production.

---

## Next Steps (Phase 3)

1. **Content System**: Implement Item CRUD with field storage
2. **Connect Host Functions**: Wire up database queries
3. **Query Engine**: Build item query API
4. **Render Pipeline**: Process RenderElement trees

---

## Appendix: Blog Plugin Exports

```bash
$ wasm-tools print plugins/blog/blog.wasm | grep "export"
  (export "memory" (memory 0))
  (export "tap_item_info" (func $tap_item_info))
  (export "tap_perm" (func $tap_perm))
  (export "tap_menu" (func $tap_menu))
  (export "tap_item_access" (func $tap_item_access))
```

All 4 taps declared in `blog.info.toml` are exported:
- `tap_item_info` - Returns ContentTypeDefinition for "blog" type
- `tap_perm` - Returns 3 permission definitions
- `tap_menu` - Returns 2 menu routes (/blog, /blog/:slug)
- `tap_item_access` - Grants access to published posts
