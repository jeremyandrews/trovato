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

## Security Rules

See `docs/security-audit.md` for the full dependency audit policy and Epic 27 story files for detailed findings.

### Format Processing

- **Always** use `FilterPipeline::for_format_safe()` for user/plugin content — never `for_format()` with untrusted format strings
- Every `| safe` usage in Tera templates requires a `{# SAFE: reason #}` comment justifying pre-sanitization

### HTML & XSS Prevention

- All user content interpolated into HTML must use `html_escape()` or Tera autoescape
- Plugin-supplied tag names must be validated against `SAFE_TAGS` in `theme/render.rs`
- Plugin-supplied attribute keys must pass `is_valid_attr_key()` in `theme/render.rs`
- Never build HTML strings with unescaped user content — use `html_escape()` from `crate::routes::helpers`

### SQL Injection Prevention

- Never use `format!()` to build SQL — always use SeaQuery parameterized queries
- JSONB path expressions: validate with `is_valid_field_name()` before interpolation
- LIKE patterns: escape `%` and `_` with `escape_like_pattern()`

### CSRF Protection

- All state-changing endpoints (POST/PUT/DELETE) must use `require_csrf` from `crate::routes::helpers`
- Logout must be POST, never GET

### Authentication & Sessions

- Password hashing: Argon2id with RFC 9106 params (m=65536, t=3, p=4) — do not weaken
- Session fixation: always call `session.cycle_id()` after authentication state changes
- Password minimum length: 12 characters — do not reduce

### WASM Plugin Boundary

- All plugin-supplied data must be validated/escaped before HTML interpolation
- Plugin database queries must use `statement_timeout` (5s) and epoch interruption (10 ticks)
- Plugin request context keys must be namespaced by plugin name

### File Upload Security

- Validate magic bytes against declared MIME type using `validate_magic_bytes()`
- Sanitize filenames with `sanitize_filename()` — never use raw user-supplied filenames in paths
- Block executable MIME types via `ALLOWED_MIME_TYPES` allowlist
- Reject disguised executables (ELF/PE with image MIME types)

### Prohibited Patterns

- `format!()` with SQL fragments
- `FilterPipeline::for_format()` with user/plugin-supplied format strings
- `| safe` without justification comment
- Unescaped user content in `write!()` / `format!()` producing HTML
- Trusting plugin-supplied tag names, class names, or attribute keys without validation

## Kernel Minimality Rules

**Governing principle:** The core kernel enables. Plugins implement. If it's a feature, it's a plugin. If it's infrastructure that plugins depend on, it's Kernel.

- See `docs/kernel-minimality-audit.md` for the full audit, extraction roadmap, LOC baseline, and quarterly review process
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
