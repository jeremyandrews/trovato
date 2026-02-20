# Story 27.5: WASM Plugin Sandbox Audit

Status: ready-for-dev

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

- [ ] Verify WASM runtime denies filesystem access (AC: #1)
  - [ ] Confirm WASI stubs return ENOSYS for fd_write
  - [ ] Confirm no fd_open/fd_read/path_open stubs exist
- [ ] Verify WASM runtime denies network access (AC: #2)
  - [ ] Confirm no socket stubs provided
  - [ ] Confirm no outbound HTTP host functions
- [ ] Audit resource limits (AC: #3)
  - [ ] Check for fuel/epoch interrupts in engine config (`runtime.rs`)
  - [ ] Document memory limits (64MB per instance, 1000 max instances)
  - [ ] Add fuel/epoch limits if absent (HIGH — prevents infinite loop DoS)
  - [ ] Add database query timeout for host DB functions
- [ ] Audit each host function for abuse potential (AC: #4)
  - [ ] DB API (`host/db.rs`): Verify DDL guard, identifier validation, parameterized queries
  - [ ] DB API: Assess arbitrary DML without row-level ACL (plugins can UPDATE any table row)
  - [ ] User API (`host/user.rs`): Verify permission checks are read-only
  - [ ] Request Context (`host/request_context.rs`): Assess plugin-to-plugin isolation
  - [ ] Cache API (`host/cache.rs`): Verify stubs are safe
  - [ ] Variables API (`host/variables.rs`): Verify stubs are safe
  - [ ] Logging (`host/logging.rs`): Assess log flooding potential
  - [ ] Item API (`host/item.rs`): Verify stubs are safe
- [ ] Verify per-request plugin isolation (AC: #5)
  - [ ] Confirm separate `Store<PluginState>` per plugin per request (`dispatcher.rs`)
  - [ ] Fix request context isolation — namespace keys by plugin name
- [ ] Fix WASI `random_get()` to use proper CSPRNG (AC: #7)
- [ ] Document all findings with severity ratings (AC: #6, #7)

## Dev Notes

### Dependencies

No dependencies on other stories. Can be worked independently.

### Codebase Research Findings

#### HIGH: No CPU/Fuel Limits on Plugin Execution

**Location:** `crates/kernel/src/plugin/runtime.rs:369-392`

Wasmtime engine configuration does not set fuel or epoch-based interrupts. A malicious or buggy plugin can run an infinite loop, blocking the HTTP request handler indefinitely. Only the HTTP-layer request timeout provides protection.

**Fix:** Enable `epoch_interruption` in engine config and set epoch deadline per invocation in `dispatcher.rs`.

#### HIGH: No Database Query Timeout

**Location:** `crates/kernel/src/host/db.rs`

Plugin DB host functions execute SQL queries without a statement timeout. A plugin can execute `SELECT * FROM large_table CROSS JOIN large_table` and block the database connection pool.

**Fix:** Set `statement_timeout` on the connection before executing plugin queries.

#### MEDIUM: Request Context Not Isolated Between Plugins

**Location:** `crates/kernel/src/host/request_context.rs:34-37`

All plugins within a request share the same `HashMap<String, String>` context. One plugin can read/overwrite another plugin's temporary state. This is a design choice for inter-plugin communication but creates a trust boundary issue.

**Fix:** Namespace context keys by plugin name (e.g., `{plugin_name}:{key}`).

#### MEDIUM: Arbitrary DML Without Row-Level ACL

**Location:** `crates/kernel/src/host/db.rs`

Plugin DB host functions (`insert`, `update`, `delete`) allow operations on any table with valid identifiers. No row-level security checks. A plugin could `DELETE FROM users WHERE true`. DDL is blocked, but DML is unrestricted.

**Mitigation options:** Database-level RLS policies, or kernel-side table ACL per plugin in `.info.toml`.

#### LOW: Pseudo-Random WASI Stub

**Location:** `crates/kernel/src/plugin/runtime.rs` (random_get stub)

`random_get()` uses predictable seed: `(buf + i) as u8).wrapping_mul(31)`. Not cryptographically secure. If plugins rely on this for security-sensitive randomness, it's exploitable.

**Fix:** Use `rand::thread_rng().fill_bytes()` from Rust std lib.

#### PROTECTED: Filesystem Access Denied

WASI stubs return ENOSYS (52) for `fd_write`. No `fd_open`, `fd_read`, `path_open` stubs exist. Plugins cannot open files.

#### PROTECTED: Network Access Denied

No socket stubs provided. WASM spec has no network primitives. Plugins would need host functions for network access.

#### PROTECTED: Memory Limits Enforced

Pooling allocator limits each instance to 64MB linear memory (`max_memory_pages * 65536`). Max 1000 concurrent instances. Enforced at Wasmtime engine level.

#### PROTECTED: DDL Guard

`host/db.rs` blocks CREATE/DROP/ALTER/TRUNCATE/GRANT/REVOKE via `is_ddl()` check. `query_raw()` only allows SELECT/WITH via `is_read_only()`. All identifiers validated via `VALID_IDENTIFIER` regex.

#### PROTECTED: Per-Request Store Isolation

Each tap invocation gets a fresh `Store<PluginState>` (dispatcher.rs:134). Separate WASM linear memory per plugin. Store dropped after execution, memory returned to pool.

### Key Files

- `crates/kernel/src/plugin/runtime.rs` — Engine config, pooling, WASI stubs
- `crates/kernel/src/host/mod.rs` — Host function registration, memory bounds checking
- `crates/kernel/src/host/db.rs` — DDL guard, identifier validation
- `crates/kernel/src/host/request_context.rs` — Shared request context
- `crates/kernel/src/tap/dispatcher.rs` — Per-request Store instantiation
- `crates/wit/kernel.wit` — Host function interface definitions

### References

- [Source: crates/kernel/src/plugin/runtime.rs — Wasmtime engine configuration]
- [Source: crates/kernel/src/host/db.rs — Plugin DB API with DDL guard]
- [Source: crates/kernel/src/host/request_context.rs — Shared context HashMap]
- [Source: crates/kernel/src/tap/dispatcher.rs — Plugin invocation isolation]
