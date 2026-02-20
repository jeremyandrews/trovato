# Story 27.5: WASM Plugin Sandbox Audit

Status: review

## Story

As a **security reviewer**,
I want the WASM plugin sandbox boundaries verified,
So that plugins cannot escape their sandbox to access the host system.

## Acceptance Criteria

1. Verified plugins cannot access host filesystem
2. Verified plugins cannot make outbound network calls
3. Resource limits (memory, CPU/fuel) on plugin execution verified or documented if absent
4. Every host function in the WIT interface audited for abuse potential
5. Inter-plugin isolation verified — one plugin cannot affect another's state
6. All findings documented with severity ratings
7. All Critical/High findings fixed

## Tasks / Subtasks

- [x] Verify WASM runtime denies filesystem access (AC: #1)
  - [x] Confirmed: `fd_write` returns ENOSYS (52)
  - [x] Confirmed: No `fd_open`, `fd_read`, `path_open` stubs exist
- [x] Verify WASM runtime denies network access (AC: #2)
  - [x] Confirmed: No socket stubs provided
  - [x] Confirmed: No outbound HTTP host functions
- [x] Audit resource limits (AC: #3)
  - [x] Memory: 64 MB per instance via pooling allocator, max 1000 instances
  - [x] CPU: Added epoch-based interruption (10-second deadline per invocation)
  - [x] DB: Added 5-second statement_timeout for all plugin queries
- [x] Audit each host function for abuse potential (AC: #4)
  - [x] DB API: DDL guard blocks CREATE/DROP/ALTER/TRUNCATE/GRANT/REVOKE
  - [x] DB API: `query_raw()` read-only guard (SELECT/WITH only)
  - [x] DB API: All identifiers validated via `VALID_IDENTIFIER` regex
  - [x] DB API: Arbitrary DML documented as acceptable (see findings)
  - [x] User API: Read-only operations only
  - [x] Cache API: Stub returns, no-op safe
  - [x] Variables API: Stub returns, no-op safe
  - [x] Item API: Stub returns, no-op safe
  - [x] Logging: Writes to tracing; rate controlled by log level
  - [x] Request Context: Namespaced by plugin name for isolation
- [x] Verify per-request plugin isolation (AC: #5)
  - [x] Confirmed: Separate `Store<PluginState>` per plugin per request
  - [x] Fixed: Request context keys now namespaced by plugin name
- [x] Fix WASI `random_get()` to use proper CSPRNG (AC: #7)
  - [x] Replaced predictable seed with `rand::thread_rng().fill_bytes()`
- [x] Document all findings with severity ratings (AC: #6, #7)

## Findings Summary

### Fixed (Critical/High)

| # | Severity | Location | Issue | Fix |
|---|----------|----------|-------|-----|
| 1 | HIGH | `runtime.rs:create_engine()` | No CPU/fuel limits. Infinite loop blocks HTTP handler indefinitely. | Enabled `epoch_interruption`, 10-second deadline per invocation, background epoch thread. |
| 2 | HIGH | `host/db.rs` | No statement timeout. Plugin can exhaust DB connections with slow queries. | Added `SET LOCAL statement_timeout = '5000'` on acquired connection before every plugin query. |

### Fixed (Medium)

| # | Severity | Location | Issue | Fix |
|---|----------|----------|-------|-----|
| 3 | MEDIUM | `host/request_context.rs` | All plugins share same context HashMap. One plugin can read/overwrite another's state. | Keys namespaced as `{plugin_name}:{key}`. Plugin name stored in `PluginState`. |
| 4 | MEDIUM | `runtime.rs:random_get()` | Uses predictable seed `((buf + i) as u8).wrapping_mul(31)`. Not cryptographically secure. | Replaced with `rand::thread_rng().fill_bytes()` (OS CSPRNG). |

### Acceptable (Low/No Fix Required)

| # | Severity | Location | Assessment |
|---|----------|----------|------------|
| 5 | MEDIUM | `host/db.rs` DML | Plugins can INSERT/UPDATE/DELETE any table row with valid identifiers. DDL blocked, WHERE required for UPDATE/DELETE, but no row-level ACL. Acceptable for trusted plugin model; plugins are admin-installed WASM bundles, not user-submitted code. Database-level RLS is a future hardening option. |
| 6 | LOW | `host/logging.rs` | Plugins can emit log messages via `tracing`. Rate controlled by configured log level. No volume limit, but operational impact only (disk fill), not security compromise. |

### Already Protected

| # | Aspect | Status | Details |
|---|--------|--------|---------|
| 7 | Filesystem access | PROTECTED | `fd_write` returns ENOSYS. No `fd_open`/`fd_read`/`path_open` stubs. |
| 8 | Network access | PROTECTED | No socket stubs. WASM spec has no network primitives. |
| 9 | Memory limits | PROTECTED | 64 MB per instance via pooling allocator. Max 1000 concurrent instances. |
| 10 | DDL guard | PROTECTED | Blocks CREATE/DROP/ALTER/TRUNCATE/GRANT/REVOKE. `query_raw` only allows SELECT/WITH. |
| 11 | Identifier validation | PROTECTED | All table/column names validated via `^[a-zA-Z_][a-zA-Z0-9_]*$` regex. |
| 12 | Per-request isolation | PROTECTED | Fresh `Store<PluginState>` per invocation. Separate WASM linear memory. Store dropped after execution. |
| 13 | Parameterized queries | PROTECTED | All user values bound via `$N` placeholders. No string interpolation of values. |

## Implementation Details

### Epoch-Based CPU Limits

Enabled `epoch_interruption(true)` in the Wasmtime engine configuration. A background thread increments the engine epoch once per second. Each plugin invocation sets `store.set_epoch_deadline(10)`, giving plugins ~10 seconds of CPU time before the engine traps with an epoch deadline error.

### Database Query Timeout

All plugin database operations (`fetch_rows_as_json` and `do_execute_raw`) now acquire an explicit connection from the pool and set `SET LOCAL statement_timeout = '5000'` before executing the query. This prevents plugins from running queries that block the connection pool. The timeout is connection-local and resets when the connection is returned to the pool.

### Request Context Isolation

Added `plugin_name: String` field to `PluginState`. The dispatcher passes the plugin name when creating the state. Request context host functions (`get` and `set`) now format keys as `{plugin_name}:{key}`, preventing one plugin from reading or overwriting another plugin's context entries.

### CSPRNG for random_get

Replaced the predictable `((buf + i) as u8).wrapping_mul(31)` fill with `rand::thread_rng().fill_bytes()`, which uses the OS-provided CSPRNG via the `rand` crate.

### Files Changed

- `crates/kernel/src/plugin/runtime.rs` — Epoch interruption config, epoch thread, CSPRNG for random_get, plugin_name in PluginState
- `crates/kernel/src/tap/dispatcher.rs` — Set epoch deadline per invocation, pass plugin name to PluginState
- `crates/kernel/src/host/db.rs` — Statement timeout on plugin DB queries
- `crates/kernel/src/host/request_context.rs` — Namespace context keys by plugin name

### Test Coverage

- All 566 unit tests pass (including plugin runtime, DB host function, dispatcher tests)
- All 82 integration tests pass
- `cargo clippy --all-targets -- -D warnings` clean
- `cargo fmt --all --check` clean
