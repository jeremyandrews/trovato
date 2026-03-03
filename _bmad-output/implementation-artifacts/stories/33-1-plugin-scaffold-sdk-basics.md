# Story 33.1: Plugin Scaffold & SDK Basics

Status: not started

## Story

As a **tutorial reader building Ritrovo**,
I want a `trovato plugin new` command that generates a working plugin scaffold,
so that I can start writing plugin logic without hand-authoring boilerplate.

## Acceptance Criteria

1. `trovato plugin new ritrovo_importer` generates a directory at `plugins/ritrovo_importer/` with:
   - `Cargo.toml` (WASM target, plugin-sdk dependency, correct crate name)
   - `src/lib.rs` with `#[no_mangle] extern "C"` entry point and stub tap registrations
   - `manifest.toml` with name, version, description, author fields
   - `migrations/` directory (empty, ready for SQL files)
2. The generated plugin compiles to `.wasm` with `cargo build --target wasm32-wasip1 --release`
3. The compiled `.wasm` is discovered by the kernel on startup (placed in or symlinked from `plugins/`)
4. The plugin appears in the admin plugin list at `/admin/plugins` with enable/disable toggle
5. `tap_plugin_install` fires on first enable; log message confirms it ran
6. Plugin registers four tap stubs: `tap_cron`, `tap_queue_info`, `tap_queue_worker`, `tap_plugin_install`
7. Tutorial section covers: WASM plugin model, tap registration, host function boundary
8. `trovato-test` blocks assert plugin discovery, tap registration, and install hook fire

## Tasks / Subtasks

- [ ] Implement `trovato plugin new <name>` CLI subcommand (AC: #1)
  - [ ] Add `New { name: String }` variant to `PluginAction` enum in `main.rs`
  - [ ] Scaffold generator writes `Cargo.toml`, `src/lib.rs`, `manifest.toml`, `migrations/`
  - [ ] Validate name is a valid Rust crate name (snake_case, no hyphens)
  - [ ] Reject if `plugins/<name>/` already exists
- [ ] Verify scaffold compiles to WASM (AC: #2)
  - [ ] `src/lib.rs` uses only `plugin-sdk` types (no std features unavailable in WASI)
  - [ ] Confirm `wasm32-wasip1` target is in the workspace toolchain file
- [ ] Wire plugin into admin UI discovery (AC: #3, #4)
  - [ ] Confirm `auto_install_new_plugins` picks up the new `.wasm` at startup
  - [ ] Plugin listed at `/admin/plugins` with correct name from `manifest.toml`
- [ ] Implement `tap_plugin_install` stub that logs on first enable (AC: #5)
- [ ] Register all four tap stubs in generated `lib.rs` (AC: #6)
- [ ] Write tutorial section 2.1 in `docs/tutorial/part-02-ritrovo-importer.md` (AC: #7)
  - [ ] WASM plugin model explanation
  - [ ] Tap registration pattern
  - [ ] Host function boundary (what plugins can call, what they can't)
- [ ] Write `trovato-test` blocks (AC: #8)

## Dev Notes

### `trovato plugin new` Implementation

The command lives in `crates/kernel/src/main.rs` → `run_plugin_command`. Add a `New` variant:

```rust
PluginAction::New { name } => cmd_plugin_new(&name).await?,
```

`cmd_plugin_new` lives in a new `crates/kernel/src/plugin/cli.rs` function. It:
1. Validates `name` matches `[a-z][a-z0-9_]*`
2. Checks `plugins/{name}/` does not exist
3. Writes the scaffold files (see templates below)
4. Prints instructions for next steps

### Scaffold Templates

**`Cargo.toml`:**
```toml
[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
trovato-plugin-sdk = { path = "../../crates/plugin-sdk" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[profile.release]
opt-level = "z"
lto = true
```

**`manifest.toml`:**
```toml
name = "{name}"
version = "0.1.0"
description = "TODO: describe this plugin"
author = "TODO"
```

**`src/lib.rs`:**
```rust
use trovato_plugin_sdk::prelude::*;

#[no_mangle]
pub extern "C" fn plugin_init() -> i32 {
    register_tap("tap_plugin_install", on_install);
    register_tap("tap_cron", on_cron);
    register_tap("tap_queue_info", on_queue_info);
    register_tap("tap_queue_worker", on_queue_worker);
    0
}

fn on_install(_ctx: &TapContext) -> TapResult {
    log_info!("{name}: plugin installed");
    TapResult::ok()
}

fn on_cron(_ctx: &TapContext) -> TapResult { TapResult::ok() }
fn on_queue_info(_ctx: &TapContext) -> TapResult { TapResult::ok() }
fn on_queue_worker(_ctx: &TapContext) -> TapResult { TapResult::ok() }
```

### Plugin SDK Entry Point

The plugin SDK is at `crates/plugin-sdk/`. The `prelude` module should expose `register_tap`, `TapContext`, `TapResult`, `log_info!`. Verify these are exported before writing the scaffold template.

### WASM Target

Check `.cargo/config.toml` or `rust-toolchain.toml` for `wasm32-wasip1` target availability. If absent, the scaffold README should include `rustup target add wasm32-wasip1`.

### Host Function Boundary

Document in tutorial: plugins run inside a WASM sandbox. They can call host functions (`log_*`, `http_request`, `db_query`, `queue_push`, `config_get`). They cannot access the filesystem, spawn threads, or open sockets directly. All I/O goes through host functions with enforced limits (statement timeout 5s, HTTP timeout 30s, queue depth cap).

### Key Files

- `crates/kernel/src/main.rs` — add `New` to `PluginAction`
- `crates/kernel/src/plugin/cli.rs` — `cmd_plugin_new` implementation
- `crates/plugin-sdk/src/lib.rs` — verify prelude exports
- `docs/tutorial/part-02-ritrovo-importer.md` — new file, section 2.1

### Dependencies

- Story 29.x (Part 1) complete — plugin system already exists
- Plugin SDK (`crates/plugin-sdk`) already has tap registration infrastructure
- `auto_install_new_plugins` in `plugin/status.rs` already handles discovery

### References

- Existing plugin examples: `plugins/blog/`, `plugins/redirects/`
- Plugin runtime: `crates/kernel/src/plugin/runtime.rs`
- Admin plugin list: `crates/kernel/src/routes/admin.rs` (plugin enable/disable)

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
