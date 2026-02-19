# Claude Code Guidelines

## Commit Messages

- Do NOT include "Co-Authored-By: Claude" or similar attribution lines
- Do NOT advertise or mention Claude/Anthropic in commit messages
- Keep commit messages focused on the technical changes only

## Code Deduplication Rules

- `html_escape` — use `crate::routes::helpers::html_escape`. Never create local copies.
- `SESSION_USER_ID` — use `crate::routes::auth::SESSION_USER_ID`. Never redefine.
- `is_valid_machine_name` — use `crate::routes::helpers::is_valid_machine_name`.
- `render_error` / `render_server_error` / `render_not_found` — use `crate::routes::helpers::{render_error, render_server_error, render_not_found}`.
- CSRF verification — use `crate::routes::helpers::require_csrf`. Never inline the pattern.
- New admin route handlers go in the appropriate `admin_*.rs` domain module, not `admin.rs`.
- New admin list/form templates should use macros from `templates/admin/macros/`.

## Coding Standards

- All code must pass `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
- See `docs/coding-standards.md` for the full reference
- New `#[allow(clippy::...)]` annotations require an explanatory comment
- All new public items must have `///` doc comments
- All new `.rs` files must have `//!` module-level documentation
- Use Trovato terminology: "category" not "taxonomy"/"vocabulary", "item" not "node", "tap" not "hook", "plugin" not "module", "gather" not "views", "tile" not "block"
- Error responses: `render_error` (400 validation), `render_server_error` (500 DB/service), `render_not_found` (404)
- `.unwrap()` forbidden in production code; use `.expect("reason")` or propagate errors

## Error Handling Rules

- `.unwrap()` forbidden in non-test production code
- `.expect("reason")` permitted with `# Panics` doc section on the enclosing function
- `write!(string, ...).unwrap()` safe on `String` — add `// Infallible:` comment
- `let _ = result` — log on failure for security operations (lockout, audit, token invalidation)
- `Response::builder().unwrap()` safe with hard-coded valid inputs — add `// Infallible:` comment
- HashMap invariant lookups: use `.expect("reason")` with `# Panics` doc, not silent `if let`
- New WASM host functions: use constants from `crates/plugin-sdk/src/host_errors.rs`
- `// SAFETY:` comments are reserved for `unsafe` blocks; use `// Infallible:` for safe-by-construction calls

## Kernel Minimality Rules

**Governing principle:** The core kernel enables. Plugins implement. If it's a feature, it's a plugin. If it's infrastructure that plugins depend on, it's Kernel.

- See `docs/kernel-minimality-audit.md` for the full audit and extraction roadmap
- **New services** in `crates/kernel/src/services/` must justify kernel placement — ask: "Does any other kernel subsystem depend on this, or only feature routes?"
- **Feature services** belong in plugins, not the kernel. If the only callers are gated routes or cron tasks, it's a feature.
- **New kernel services** that are plugin-optional must use the `Option<Arc<ServiceType>>` pattern in `AppState`
- **New plugin-specific routes** must be gated via `plugin_gate!` macro in `routes/mod.rs` + `GATED_ROUTE_PLUGINS` in `plugin/gate.rs`
- **New cron tasks** for plugin features should use `tap_cron` (when activated) rather than hardcoded kernel cron tasks
- **Infrastructure services** (depended on by multiple kernel subsystems or all plugins) stay in kernel: content, gather, search, files, cache, forms, permissions, stages, auth

## Before Committing Checklist

1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --all`
