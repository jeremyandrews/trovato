# Trovato Coding Standards

## Quick Start

New to the codebase? Follow these five rules and you'll be fine:

1. **Run the toolchain before committing:** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all`
2. **Use Trovato terminology:** "category" not "taxonomy", "item" not "node", "tap" not "hook", "plugin" not "module", "gather" not "views", "tile" not "block"
3. **Never `.unwrap()` in production code.** Use `.expect("reason")` or propagate errors with `?`.
4. **Reuse shared helpers.** Check `crate::routes::helpers` before writing HTML escaping, CSRF verification, error rendering, or login checks.
5. **New admin routes go in `admin_*.rs` domain modules**, not `admin.rs`.

---

## Formatting

### Configuration

The workspace uses `.rustfmt.toml` at the repository root:

```toml
edition = "2024"
use_field_init_shorthand = true   # Foo { x: x } → Foo { x }
use_try_shorthand = true          # try!(expr) → expr?
```

Import grouping (`group_imports = "StdExternalCrate"`) and merging (`imports_granularity = "Crate"`) are configured but commented out — they require nightly rustfmt. When CI adopts nightly for formatting, these will be enabled.

### Import Ordering Convention

Until automatic enforcement is available, follow this convention manually:

```rust
// 1. Standard library
use std::collections::HashMap;

// 2. External crates
use axum::extract::State;
use serde::Deserialize;

// 3. Local crates / current crate
use crate::models::Item;
use crate::routes::helpers::render_error;
```

### Running the Formatter

```sh
cargo fmt --all          # Format everything
cargo fmt --check        # CI check (no modifications)
```

---

## Linting

### Configuration

Workspace-level lint configuration lives in the root `Cargo.toml`:

```toml
[workspace.lints.clippy]
all = "warn"
manual_let_else = "warn"
uninlined_format_args = "warn"
semicolon_if_nothing_returned = "warn"
```

Each member crate inherits these via `[lints] workspace = true`.

The `clippy.toml` file raises thresholds for complex kernel functions:

```toml
too-many-arguments-threshold = 8
type-complexity-threshold = 300
```

### `#[allow]` Annotation Rules

- Every `#[allow(clippy::...)]` annotation **must** have an explanatory comment on the preceding line.
- Crate-level `#![allow(...)]` attributes are reserved for legitimate cases where large sections of code trigger the warning by design (e.g., `dead_code` in lib.rs for items exposed to tests/plugins).
- Do not blanket-allow warnings. Prefer fixing the code.

**Good:**
```rust
// False positive: the match arm binds to prevent accidental fallthrough.
#[allow(clippy::match_single_binding)]
fn process(input: &str) { ... }
```

**Bad:**
```rust
#[allow(clippy::all)]  // Never do this
fn messy_function() { ... }
```

### Running Clippy

```sh
cargo clippy --all-targets                # Development (warnings)
cargo clippy --all-targets -- -D warnings # CI (deny all warnings)
```

---

## Naming Conventions

### Trovato Terminology

Trovato has its own domain language. Use these terms consistently in code, comments, documentation, UI strings, and URLs:

| Trovato Term | Do NOT Use     | Notes                                  |
|-------------|----------------|----------------------------------------|
| **category** | taxonomy, vocabulary | Hierarchical classification system |
| **item**     | node, entity   | Core content record                    |
| **tap**      | hook           | Plugin extension point                 |
| **plugin**   | module         | WASM extension package                 |
| **gather**   | views, view    | Saved query / listing configuration    |
| **tile**     | block          | Renderable UI region component         |

### Rust Naming

Follow standard Rust conventions:
- Types: `PascalCase` (`ItemService`, `CategoryTree`)
- Functions/methods: `snake_case` (`render_error`, `require_admin`)
- Constants: `SCREAMING_SNAKE_CASE` (`SESSION_USER_ID`, `MAX_UPLOAD_SIZE`)
- Machine names (stored in DB): lowercase with underscores (`blog`, `audit_log`)
- URL paths: lowercase with hyphens (`/admin/content/items`, `/admin/structure/categories`)

---

## Plugin Conventions

### Directory Layout

```
plugins/
  my_plugin/
    Cargo.toml              # crate-type = ["cdylib"], [lints] workspace = true
    my_plugin.info.toml     # Required metadata and tap declarations
    src/
      lib.rs                # All tap implementations + tests
    migrations/             # Optional SQL migration files
      001_create_table.sql
```

### `.info.toml` Format

```toml
name = "my_plugin"
description = "Short description of the plugin"
version = "1.0.0"
dependencies = []           # Other plugin names this depends on

[taps]
implements = [
    "tap_item_info",
    "tap_menu",
    "tap_perm",
]
weight = 0                  # Execution order; lower = earlier

[migrations]
files = [
    "migrations/001_create_table.sql",
]
```

### Tap Functions

Use the proc macro attributes from the SDK:

```rust
use trovato_sdk::prelude::*;

#[plugin_tap]                   // For infallible taps
fn tap_item_info() -> Vec<ItemTypeInfo> { ... }

#[plugin_tap_result]            // For taps that can fail
fn tap_item_presave(item: &mut Item) -> Result<(), String> { ... }
```

### Build Targets

Plugins compile to WASM:

```sh
cargo build --target wasm32-wasip1 -p my_plugin
```

### Testing with `__inner_*`

The `#[plugin_tap]` macro generates a private `__inner_<tap_name>` function that can be tested without WASM export overhead:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_info_returns_one_type() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].machine_name, "blog");
    }
}
```

---

## Error Handling

### Response Helpers

Three functions in `crate::routes::helpers` cover all error responses:

| Function             | Status | When to Use                                       |
|---------------------|--------|---------------------------------------------------|
| `render_error(msg)` | 400    | Client validation failures, CSRF errors, bad input |
| `render_server_error(msg)` | 500 | Database failures, service errors, unexpected panics |
| `render_not_found()` | 404   | Missing resources, unknown routes                  |

All escape HTML internally to prevent XSS. Never build error HTML manually.

### CSRF Verification

Always use the shared helper:

```rust
use crate::routes::helpers::require_csrf;

if let Err(resp) = require_csrf(&session, &form.token).await {
    return resp;
}
```

Never inline CSRF verification logic.

### `.unwrap()` Policy

- **Production code:** Forbidden. Use `.expect("descriptive reason")` or propagate with `?`.
- **Tests:** `.unwrap()` is acceptable — test failures already produce stack traces.
- **Static guarantees:** `.unwrap()` is acceptable when the value is provably `Some`/`Ok` (e.g., a regex literal). Add a comment explaining why.

### `.expect()` Policy

- Permitted when the invariant is statically guaranteed (e.g., HKDF output size, built-in theme lookup, HashMap key inserted earlier).
- Must include a `# Panics` doc section on the enclosing function (not struct/type).
- Prefer error propagation with `?` when the caller can handle the error.

### `let _ =` Policy

- Acceptable for fire-and-forget operations where the caller cannot meaningfully handle the error.
- **Security operations** (lockout recording, audit logging, token invalidation): log on failure with `tracing::warn!`.
- Use `if let Err(e) = ... { tracing::warn!(...) }` instead of `let _ =` for security-relevant calls.

### `write!` to `String`

- `write!(string, ...).unwrap()` is safe — the `fmt::Write` impl for `String` is infallible.
- Add an inline `// Infallible: write!() to String is infallible` comment.
- Does **not** require a `# Panics` doc section — the infallibility is a universal language guarantee, not a design-specific invariant. Use `# Panics` only for `.expect()` calls where the invariant depends on surrounding code or domain logic.
- Reserve `// SAFETY:` comments exclusively for `unsafe` blocks.

### WASM Host Function Error Codes

All WASM host functions follow a standard error code convention documented in
`crates/plugin-sdk/src/host_errors.rs`. New host functions must use the constants
(`ERR_MEMORY_MISSING`, `ERR_PARAM1_READ`, etc.) instead of raw integer literals.

---

## Documentation

### Module-Level (`//!`)

Every `.rs` file should have a module-level doc comment explaining its purpose:

```rust
//! Item model and CRUD operations.
//!
//! Items are the core content records in Trovato.
//! They support JSONB field storage and revision history.
```

### Public Items (`///`)

All public functions, structs, enums, and traits must have doc comments. Focus on **why** not **what** — the signature already tells readers what the function takes and returns.

**Good:**
```rust
/// Require an authenticated admin user, or redirect/reject.
///
/// Returns the admin [`User`] on success. Redirects to `/user/login` if the
/// session has no valid user id. Returns 403 if the user exists but is not
/// an admin.
pub async fn require_admin(state: &AppState, session: &Session) -> Result<User, Response> {
```

**Bad:**
```rust
/// This function takes an AppState and Session and returns a Result.
pub async fn require_admin(state: &AppState, session: &Session) -> Result<User, Response> {
```

### Struct Fields

Document non-obvious fields:

```rust
pub struct Item {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,
    /// Publication status (0 = unpublished, 1 = published).
    pub status: i16,
}
```

---

## Code Organization

### Admin Routes

Admin route handlers live in domain-specific files:

| File                    | Responsibility                         |
|------------------------|----------------------------------------|
| `admin.rs`             | Dashboard, stage management, file ops, comment moderation |
| `admin_content.rs`     | Item CRUD, revision management         |
| `admin_content_type.rs`| Content type add/edit/delete           |
| `admin_taxonomy.rs`    | Category and tag management            |
| `admin_alias.rs`       | URL alias management                   |
| `admin_user.rs`        | User administration                    |
| `gather_admin.rs`      | Gather query management                |
| `tile_admin.rs`        | Tile layout management                 |
| `plugin_admin.rs`      | Plugin enable/disable                  |

New admin functionality goes in the appropriate `admin_*.rs` file, or a new one if no existing file fits. Never add route handlers to `admin.rs` unless they are core dashboard functionality.

### Templates

Admin templates use shared macros from `templates/admin/macros/` for consistency. Use these macros for admin list pages, forms, and common UI patterns rather than duplicating HTML.

### Shared Helpers

Common utilities live in `crate::routes::helpers`:

- `html_escape()` — HTML entity escaping (never create local copies)
- `require_login()` / `require_admin()` — authentication gates
- `require_csrf()` — CSRF token verification
- `render_error()` / `render_server_error()` / `render_not_found()` — error responses
- `is_valid_machine_name()` — machine name validation
- `render_admin_template()` — render an admin page with standard layout and context
- `inject_site_context()` — common Tera template context
- `CsrfOnlyForm` — generic form struct for action-only POST endpoints

Session constants live in `crate::routes::auth`:

- `SESSION_USER_ID` — session key for the authenticated user ID

---

## Manual Review Checklist

Things CI can't catch — reviewers should verify:

1. **Terminology compliance** — no Drupal-isms in new code, comments, or UI strings
2. **Helper reuse** — shared helpers used instead of inline reimplementations
3. **Error response correctness** — 400 vs 500 vs 404 used appropriately
4. **Admin route placement** — handlers in the right `admin_*.rs` file
5. **Template macro usage** — admin UI uses shared macros, not bespoke HTML
6. **`.unwrap()` justification** — any `.unwrap()` in non-test code has a clear reason
7. **Documentation quality** — public API docs explain "why", not just "what"
8. **Security** — user input escaped, CSRF verified on all state-changing POST routes, no SQL concatenation
9. **Plugin testing** — `__inner_*` functions tested, not just WASM exports
10. **Machine names** — lowercase with underscores, validated with `is_valid_machine_name()`
