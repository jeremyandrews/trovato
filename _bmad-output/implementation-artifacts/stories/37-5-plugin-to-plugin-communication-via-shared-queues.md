# Story 37.5: Plugin-to-Plugin Communication via Shared Queues

Status: done

## Story

As a **plugin developer**,
I want to enqueue work items from one plugin and have them processed by another,
so that plugins can communicate asynchronously through a shared queue infrastructure.

## Acceptance Criteria

1. `queue_push` host function available to WASM plugins via `trovato:kernel/queue` namespace
2. Queue push accepts queue_name, payload (JSON), and auto-injects plugin_name from PluginState
3. Payload is validated as well-formed JSON before insertion
4. Queue items stored in `plugin_queue` table with plugin_name, queue_name, payload, created_at
5. Plugins cannot impersonate each other (plugin_name comes from PluginState, not plugin input)
6. Error codes returned for common failures: no memory export (-1), invalid queue name (-2), invalid payload (-3), invalid JSON (-4), DB failure (-5)
7. Kernel cron drains the queue and dispatches `tap_queue_worker` to the owning plugin

## Tasks / Subtasks

- [x] Register queue_push host function in Wasmtime linker (AC: #1)
- [x] Read queue_name and payload from WASM linear memory (AC: #2)
- [x] Validate payload as JSON before DB insert (AC: #3)
- [x] Insert into plugin_queue table with auto-injected plugin_name (AC: #4, #5)
- [x] Return error codes for each failure mode (AC: #6)
- [x] Create plugin_queue table migration (AC: #4)
- [x] Wire queue drain and tap_queue_worker dispatch in cron (AC: #7)

## Dev Notes

### Architecture

The queue host function (`host/queue.rs`) provides the bridge between WASM plugin sandboxes and the shared database-backed queue. Key design decisions:

- **Security**: `plugin_name` is injected from `PluginState` (set at plugin load time), not from plugin-supplied data. This prevents plugins from pushing items that appear to come from other plugins.
- **Validation**: Payload is parsed as `serde_json::Value` before insert to reject malformed JSON early. This prevents the queue from being polluted with unparseable entries.
- **Async**: Uses `func_wrap_async` for the Wasmtime linker binding since the DB insert is async.
- **Error handling**: Negative integer return codes (-1 through -5) for different failure modes, with 0 indicating success. Errors are logged with `tracing::warn`.

The `plugin_queue` table schema: `(id SERIAL, plugin_name TEXT, queue_name TEXT, payload JSONB, created_at BIGINT, processed_at BIGINT NULL)`. The cron worker selects unprocessed items, dispatches `tap_queue_worker`, and marks them processed.

### Testing

- Queue push tested via plugin integration tests
- Error codes tested for invalid inputs
- Queue drain tested via cron worker integration

### References

- `crates/kernel/src/host/queue.rs` -- queue_push host function registration
- `crates/kernel/src/plugin/` -- PluginState with plugin_name
