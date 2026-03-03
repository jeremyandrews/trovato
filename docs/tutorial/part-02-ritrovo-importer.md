# Part 2: The Ritrovo Importer Plugin

In Part 1, you built a Trovato site and manually created three conferences. That works for demos, but Ritrovo needs hundreds of real conferences — pulled automatically from the open-source [confs.tech](https://github.com/tech-conferences/conference-data) dataset.

In this part you'll build the `ritrovo_importer` plugin: a WASM module that runs on a daily cron cycle, fetches conference JSON from GitHub, and keeps your database up to date. Along the way you'll learn how Trovato's plugin system works and when to reach for it.

---

## 2.1 The WASM Plugin Model

### What is a plugin?

A Trovato plugin is a WebAssembly module (`.wasm` file) that the kernel discovers at startup, loads into an isolated sandbox, and calls at specific lifecycle points called **taps**.

Plugins live in `plugins/{name}/` as Rust `cdylib` crates. They compile to WASM and are discovered automatically — drop a `.wasm` next to an `.info.toml` manifest and the server picks it up.

### Why WASM?

The WASM sandbox enforces hard limits:

| Resource | Limit |
|---|---|
| Database query timeout | 5 s |
| HTTP request timeout | 30 s |
| Clock ticks (epoch interruption) | 10 ticks |

If a plugin hangs, the kernel kills it without affecting the rest of the site. This is the same reason browsers run untrusted JavaScript in a sandbox — isolation is the point.

Plugins cannot access the filesystem, spawn threads, or open sockets directly. All I/O goes through **host functions**: `db_query`, `http_request`, `queue_push`, `log`, and a handful of others. The kernel controls what plugins can do.

### Taps

A tap is a function your plugin exports that the kernel calls at a specific moment. Think of them like webhooks, but in-process and sandboxed.

```
Kernel event → serialise inputs to JSON → call plugin tap → deserialise JSON result
```

You declare which taps you implement in `{name}.info.toml`:

```toml
[taps]
implements = ["tap_install", "tap_cron", "tap_perm", "tap_menu"]
```

In Rust, each tap is a regular function annotated with `#[plugin_tap]`. The macro generates the WASM export boilerplate — reading JSON from WASM memory, calling your function, writing the result back:

```rust
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    // your logic here
    serde_json::json!({ "status": "ok" })
}
```

### Scaffolding a new plugin

The `trovato plugin new` command generates the boilerplate for you:

```
trovato plugin new my_plugin
```

This creates:

```
plugins/my_plugin/
  Cargo.toml               # cdylib crate, trovato-sdk dependency
  my_plugin.info.toml      # manifest: name, version, taps
  src/lib.rs               # stub implementations for 4 taps
  migrations/              # empty, ready for SQL migration files
```

It also adds `"plugins/my_plugin"` to the workspace `Cargo.toml` members list.

> **Note:** The `ritrovo_importer` plugin already ships with Trovato as a complete example. You don't need to scaffold it — instead, read through its source to understand the patterns, then use `trovato plugin install ritrovo_importer` to enable it.

### Installing and enabling a plugin

After building the `.wasm`:

```bash
cargo build --target wasm32-wasip1 -p ritrovo_importer --release
trovato plugin install ritrovo_importer
```

`plugin install` runs any pending SQL migrations, then marks the plugin as enabled. The next time the server starts (or via the admin UI at `/admin/plugins`), the plugin loads.

When the plugin is enabled **for the first time**, the kernel calls `tap_install`. You'll see this in the server logs:

```
INFO ritrovo_importer: ritrovo_importer installed — import will begin on next cron cycle
```

> **Note:** `tap_install` fires only once — on first enable. If `ritrovo_importer` was already installed automatically at server startup (the default), you won't see this message in existing environments. To see it, disable the plugin, uninstall it via the admin UI, re-enable it, and watch the logs.

### The four stubs: tap_install, tap_cron, tap_queue_info, tap_queue_worker

The generated scaffold includes stubs for the four taps the importer uses:

| Tap | When called | What it does |
|---|---|---|
| `tap_install` | Once, on first enable | Seeds initial data, logs confirmation |
| `tap_cron` | Every cron cycle (~1 min) | Fetches conference data, pushes to queue |
| `tap_queue_info` | At startup | Declares queue names and concurrency |
| `tap_queue_worker` | Per queue job | Validates and inserts/updates conferences |

The next section covers how `tap_cron` and the queue work together.

> **Note:** The `ritrovo_importer` exemplar plugin declares only `tap_install`, `tap_cron`, `tap_perm`, and `tap_menu` — not the queue taps. The queue infrastructure is introduced in section 2.2. The scaffold includes all four tap stubs upfront so you don't have to add them later.
