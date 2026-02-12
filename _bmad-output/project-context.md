---
project_name: 'trovato'
user_name: 'Jeremy'
date: '2026-02-11'
sections_completed: ['technology_stack', 'rust_rules', 'plugin_system', 'testing', 'code_quality', 'workflow', 'critical_rules', 'fundamental_laws']
status: 'complete'
rule_count: 120+
optimized_for_llm: true
---

# Project Context for AI Agents

_This file contains critical rules and patterns that AI agents must follow when implementing code in this project. Focus on unobvious details that agents might otherwise miss._

---

## Technology Stack & Versions

**Core:**
- Rust Edition 2024 (workspace-level) — do not downgrade to 2021
- Axum 0.8 + Tokio 1.x (full features) for async HTTP
- Wasmtime 28 for WASM plugin runtime
- PostgreSQL (SQLx 0.8) with JSONB field storage
- Redis 0.27 for sessions, cache, distributed locks
- SeaQuery 0.32 for type-safe query building (Gather engine)

**Plugin Compilation:**
- Target: `wasm32-wasip1` (WASI Preview 1, core modules)
- **Do NOT use Component Model or WASI Preview 2 patterns** — this project uses core modules only
- Crate type: `cdylib` — plugins MUST include `crate-type = ["cdylib"]` in `[lib]`
- Requires: `rustup target add wasm32-wasip1` before building plugins

**Key Dependencies:**
- Tera 1.x (templating), Moka 0.12 (L1 cache), Argon2 0.5 (auth)
- UUID v7 (time-ordered) — use `Uuid::now_v7()`, not `Uuid::new_v4()`
- Chrono 0.4 (serde feature enabled), Thiserror 2 (new major version)
- Tracing 0.1 + tracing-subscriber 0.3 for observability

**Workspace Pattern (CRITICAL):**
- All dependency versions defined in root `Cargo.toml` under `[workspace.dependencies]`
- Crates MUST use `{ workspace = true }` to inherit versions
- **Never add versions directly in crate Cargo.toml files**

**Async Constraints:**
- Wasmtime Store is `!Send` and `!Sync`
- Use pooled instantiation model — one Store per request
- Do not hold Store references across await points outside the request handler

**DB Access Boundary:**
- Kernel code: uses SQLx directly
- Plugin code: uses structured `db` WIT interface (prevents SQL injection)
- Raw SQL in plugins requires explicit `raw_sql` permission in `.info.toml`

## Rust-Specific Rules

**Ownership & Memory:**
- No borrowing across WASM boundary — all data crossing is owned (`String`, not `&str`)
- `ItemHandle` is `Copy` (just an i32 index); SDK structs like `RenderElement` require `.clone()`
- Validate at construction — constructors enforce invariants, not runtime checks

**Error Handling:**
- **Never panic in plugin code** — use `?` propagation or explicit `match`
- Kernel catches panics but loses context; prefer `Result<(), String>` returns
- View taps return error RenderElements on failure, not panics

**Async/Await:**
- All database operations are async (SQLx)
- Use `#[tokio::main]` for binaries, `#[tokio::test]` for async tests
- **Never use `block_on` inside Tokio runtime** — causes deadlock
- Use `tokio::task::spawn_blocking` for CPU-intensive work in async contexts

**Performance Awareness:**
- `tap_item_view` runs ~500x per page render — avoid unnecessary allocations in hot paths
- Prefer `&str` parameters in builders; allocate only at `.build()`
- Don't add `#[inline]` manually — compiler optimizes, WASM doesn't benefit

**Code Style:**
- Import order: `std` → external crates → local crates → `prelude::*`
- Use `trovato_sdk::prelude::*` for SDK types; explicit imports for serde, etc.
- Required parameters in constructors, optional via builder method chains
- Use `thiserror` 2.x for custom error types in Kernel code

## WASM Plugin System Rules

**Security Model (Plugins are Untrusted):**
- Plugins return JSON RenderElements, **never raw HTML** — Kernel sanitizes
- Only use known `#format` values: omit (escaped), `plain_text`, `filtered_html`
- Structured DB API prevents SQL injection; `query-raw` requires explicit permission
- No outbound network access from plugins (no HTTP host functions)
- Never include internal details (paths, SQL, traces) in user-facing errors

**Plugin Structure:**
- Each plugin: `plugins/{name}/` with `{name}.info.toml` + `src/lib.rs`
- `.info.toml` is source of truth — Kernel reads this, not plugin introspection
- Mismatch between `.info.toml` and exports causes `MissingExport` at first call
- Every plugin needs manifest — no `.info.toml` = invisible to Kernel
- Dependencies form a DAG — circular references are fatal errors
- Rebuild plugins after SDK changes (no ABI stability yet)

**Tap Conventions:**
- All tap functions use `tap_` prefix (tap_item_view, tap_menu, tap_perm)
- Keep `.info.toml` implements array in sync with `#[plugin_tap]` functions
- Taps execute in weight order (lower = earlier, default 0)
- Mutations accumulate — Plugin B sees changes from Plugin A
- Alter taps modify existing tree — **never replace root element entirely**

**Handle vs Full Serialization:**
- Default: `data_mode = "handle"` — Kernel passes `ItemHandle` (i32)
- Opt-in: `data_mode = "full"` — Kernel passes full JSON
- **Use handle-based unless restructuring entire item** — 5x+ faster
- Proc macro infers mode from function signature: `&ItemHandle` vs `&Item`

**Handle Safety (CRITICAL):**
- Handles valid ONLY within current request — indices reused after
- **Never store ItemHandle in static/global variables**
- `get_field<T>` returns `None` on type mismatch — always handle Option

**Store Lifecycle:**
- Module compiled once (shared), Store pooled per-request (~5µs)
- **Never cache Store or Instance across requests** — pool handles this
- Plugins instantiate lazily — only when their tap is invoked
- Same Store reused for multiple taps within one request

**Performance Rules:**
- **NEVER query inside a loop** in tap_item_view — use batch queries with IN clause
- tap_item_view runs ~500x per page — O(n) queries become O(n²) disaster
- Never cache unbounded collections — paginate or limit size
- Test with production-scale data (thousands of items, not tens)

**Lifecycle Tap Safety:**
- install/enable/disable/uninstall must **NEVER panic**
- Use `IF EXISTS` / `IF NOT EXISTS` for all DDL operations
- Test lifecycle taps in fresh AND existing environments
- disable must succeed even if install was never called

**Error Handling in Plugins:**
- Never use `.unwrap()` — use `.unwrap_or_default()` or `?`
- Handle empty query results — zero rows is normal, not exceptional
- Plugin errors should degrade gracefully — show fallback, not crash page

**Input/Output Safety:**
- Never construct query JSON via string formatting — use SDK builders
- Always use `render::` builder API — never hand-craft RenderElement JSON
- All strings must be valid UTF-8 — Rust String guarantees this
- When caching user-specific data, include user context in cache key

## Testing Rules

**Two-Layer Testing Model:**
- **MockKernel** (unit): Test plugin logic — fast, no I/O, write 10x more of these
- **TestEnvironment** (integration): Test real WASM loading — slower, catches boundary bugs
- Plugin unit tests compile to native target, not WASM — they test logic only
- Integration tests via TestEnvironment test the actual `.wasm` file

**Test Coverage Requirements:**
- Every tap function: at least one happy path + one error path test
- `tap_item_view`: test with 0, 1, and many items/fields
- Lifecycle taps: test fresh install AND upgrade scenarios

**Test Data Patterns:**
- Use test builders from `test-utils`, not raw JSON construction
- Builders catch schema changes at compile time
- Transaction rollback for cleanup in integration tests — not DELETE statements

**Async Test Rules:**
- Use `#[tokio::test]` for all async tests
- Await all spawned tasks — don't let background work escape test boundaries
- No timeouts in tests — use explicit assertions

**Test Quality:**
- Flaky tests must be fixed or deleted — never `#[ignore]` without tracking issue
- Test names describe behavior: `tap_item_view_returns_title_in_render_element`
- Use `assert_eq!` with clear messages, not `.unwrap()` for assertions

**Definition of Done:**
- All taps have unit tests (MockKernel)
- At least one integration test proves plugin loads
- Tests pass in CI, not just locally

## Code Quality & Style Rules

**Tooling (Non-Negotiable):**
- Run `cargo fmt` before every commit — no formatting debates
- Run `cargo clippy` and fix all warnings — no `#[allow()]` without justifying comment
- Run `cargo test` — all tests pass before commit

**Naming Conventions:**
- **Use Trovato terminology** from `docs/design/Terminology.md` — never Drupal terms
  - `item` not `node`, `tap` not `hook`, `plugin` not `module`, `stage` not `workspace`
- Functions: verb_noun (`item_load`), is_adjective (`is_published`), get_noun (`get_title`)
- Types: PascalCase nouns (`ItemHandle`, `AccessResult`)
- Constants: SCREAMING_SNAKE_CASE — extract magic numbers to named constants
- Fields: `field_{name}` prefix for content type fields
- Tables: snake_case singular (`item`, `category_term`)

**Visibility & API Design:**
- Default to private; `pub(crate)` for internal sharing; `pub` only for true public API
- Never `pub` just for test access — use `#[cfg(test)]` modules
- Mark experimental APIs with `#[doc(hidden)]` and document stability

**File & Module Organization:**
- Type name matches file name: `content_type.rs` → `ContentType`
- One primary public type per module — split if exporting many types
- Keep module hierarchy shallow (max 2-3 levels)

**Documentation:**
- All public items need doc comments explaining **why**, not just **what**
- Include `# Example`, `# Panics`, `# Errors` where applicable
- WIT interface comments are mandatory

**Error & Log Messages:**
- Errors: lowercase, no trailing punctuation, include context
  - Good: `failed to load item {id}: field 'title' missing`
  - Bad: `Error: Failed to load item.`
- Logging: use `tracing` with structured fields
  - Good: `tracing::info!(item_id = %id, "item loaded")`
  - Bad: `tracing::info!("item {} loaded", id)`

**Safety & Soundness:**
- No `unsafe` in plugin code — WASM sandbox makes it unnecessary
- Kernel `unsafe` requires `// SAFETY:` comment explaining invariants
- Never commit `todo!()` to main — it panics at runtime
- Use `// TODO(name):` for planned work, `// FIXME:` for known bugs

**Dependencies:**
- Check existing workspace deps before adding new ones
- Prefer stdlib over external crates
- No git dependencies in production code
- New deps require justification in PR description

**Conditional Compilation:**
- `#[cfg(target_arch = "wasm32")]` for plugin-specific code
- No `#[cfg(feature = ...)]` in plugins — features unsupported in WASM

## Development Workflow Rules

**Build Commands:**
- Kernel: `cargo build -p trovato-kernel`
- Single plugin: `cargo build --target wasm32-wasip1 -p {plugin-name}`
- All plugins: `scripts/build-plugins.sh` (iterates plugins directory)
- Full rebuild: `cargo build --workspace && scripts/build-plugins.sh`
- **`cargo build --workspace` does NOT build WASM plugins**

**Rebuild Dependencies:**
| Changed | Must Rebuild |
|---------|--------------|
| Plugin code | That plugin only |
| Kernel code | Kernel only |
| SDK types | SDK + all plugins + kernel |
| WIT interface | SDK + all plugins + kernel |

**WIT Change Detection:**
- Plugins need `build.rs` with `cargo:rerun-if-changed` for WIT files
- Without this, Cargo won't detect WIT changes and uses stale bindings

**Local Setup Prerequisites:**
- `rustup target add wasm32-wasip1`
- `rust-toolchain.toml` pins exact Rust version — all devs use same version
- Docker or local Postgres + Redis for integration tests
- Document everything in `CONTRIBUTING.md`

**CI Pipeline:**
- `cargo fmt --check` — CI rejects, never auto-formats
- `cargo clippy -- -D warnings` — warnings are errors in CI
- `cargo test --workspace` — debug mode
- `cargo test --workspace --release` — also test release mode
- `scripts/build-plugins.sh` — verify all plugins compile
- `cargo tree -d` — duplicate dependencies = build failure

**SQLx Offline Mode:**
- Use `cargo sqlx prepare` and commit `.sqlx/` directory
- CI builds in offline mode — no database needed for compilation
- Local dev can use live database for immediate feedback

**Version Control:**
- Commit `Cargo.lock` for binaries (kernel, benchmarks)
- Branch from `main`, PR required, squash merge
- Commit format: `type(scope): description (Design-Doc §section)`

**Phase Gates (Blocking):**
- Phase N must pass gate before starting Phase N+1
- Document completion in `docs/phase-gates/phase-{n}-complete.md`
- Each phase ends with runnable demo, not just "code complete"

**Database Migrations:**
- Schema changes in numbered migration files only
- Never ad-hoc `ALTER TABLE` in plugin install taps
- Migrations run before Kernel starts, not during

**Async Task Hygiene:**
- Every `tokio::spawn` must have handle awaited before scope ends
- Use `JoinSet` for multiple spawned tasks
- No fire-and-forget spawns in tests

**Error Context:**
- Every `?` crossing module boundary adds context via `map_err`
- Naked `?` only within same function

**Release Profile:**
- Enable `overflow-checks = true` in release profile
- Run `cargo fix --edition` before bumping Rust edition

## Critical Don't-Miss Rules

**The Critical Eight (Violation = Disaster):**
1. **Never store ItemHandle across requests** — indices reuse, stale handles corrupt
2. **Never query inside a loop** — tap_item_view × 500 items × N queries = death
3. **Never `.unwrap()` in plugins** — use `?` or `.unwrap_or_default()`
4. **Never return raw HTML** — JSON RenderElements only, Kernel sanitizes
5. **Never bypass workspace deps** — all versions in root `Cargo.toml`
6. **Never forget `IF EXISTS`/`IF NOT EXISTS`** in lifecycle tap DDL
7. **Never commit `todo!()` or `unreachable!()`** — they panic at runtime
8. **Never index arrays directly** — use `.get(i)` and handle `None`

**All Panic Sources (Banned in Plugins):**
- `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, `unimplemented!()`, `unreachable!()`
- `assert!()` outside of tests
- Direct indexing `arr[i]` — use `arr.get(i)`
- Unchecked slice operations

**Terminology (Enforced):**
- Trovato terms only: item, tap, plugin, gather, tile, categories, record, slot, stage
- Never: node, hook, module, views, block, taxonomy, entity, region, workspace
- CI grep check for banned terms recommended

**Security Boundaries:**
- Plugins are sandboxed — no filesystem, no network, no raw SQL by default
- User input: never use directly as cache keys, log messages, or field names
- Validate JSON structure before `set_field_json`

**Rebuild Triggers (Don't Skip):**
- WIT file changed → rebuild SDK + all plugins + kernel
- SDK changed → rebuild all plugins + kernel
- Forgetting this = mysterious runtime errors from stale bindings

**The Integration Test Rule:**
- No plugin is "done" until integration test loads actual `.wasm` file
- Unit tests don't catch WIT mismatches — only integration tests do

---

## Fundamental Laws (The Architecture Exists Because Of These)

**Law 1: The WASM Boundary Is Real**
- All data crossing is copied, never borrowed
- Handles are indices, not pointers — valid only while host says so
- Capabilities are exactly what WIT defines — no more, no less

**Law 2: Requests Are Isolated Units**
- Nothing survives request end unless explicitly persisted
- ItemHandle from request A is invalid in request B
- WASM memory resets between requests (pooled Store model)

**Law 3: Concurrency Requires Isolation**
- One Store per request — never shared
- Shared state needs synchronization (DB transactions, Redis locks)
- Plugins cannot coordinate except through host-provided mechanisms

**Law 4: Untrusted Code Is Contained**
- Plugins never produce final HTML — only JSON RenderElements
- Kernel catches panics — plugin failures don't crash server
- Permissions are deny-by-default — Kernel enforces all grants

**Law 5: Scale Reveals Complexity**
- O(1) per call or die — tap_item_view runs 500x per page
- Batch before the loop, not inside — 1 query for 50 items, not 50 queries
- Bound all caches — unbounded = memory exhaustion

**If You Fight These Laws, You Lose.**

---

## Usage Guidelines

**For AI Agents:**
- Read this file before implementing any code in this project
- Follow ALL rules exactly as documented
- When in doubt, prefer the more restrictive option
- The "Critical Eight" and "Fundamental Laws" are non-negotiable

**For Humans:**
- Keep this file lean and focused on agent needs
- Update when technology stack or patterns change
- Review quarterly for outdated rules
- Remove rules that become obvious over time

**Reference Documents:**
- `docs/design/Terminology.md` — Drupal → Trovato naming map
- `docs/design/*.md` — Detailed design specifications
- `crates/wit/kernel.wit` — WIT interface contract

---

*Last Updated: 2026-02-11*

