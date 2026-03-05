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
- LIKE patterns: escape `%` and `_` with `escape_like_wildcards()`

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

## Database Backups

`pg_dump` is not on the default PATH — use the full path from Homebrew's `libpq` formula.
Use `$(brew --prefix libpq)/bin/` rather than a hardcoded version path:

```bash
# Backup (custom format, ~100K for a tutorial-sized DB)
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/<name>-$(date +%Y%m%d).dump

# Restore — drop and recreate for a clean slate (--clean only drops objects IN the dump,
# leaving behind tables added after the snapshot was taken)
$(brew --prefix libpq)/bin/dropdb --if-exists postgres://trovato:trovato@localhost:5432/trovato
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/postgres \
  -c "CREATE DATABASE trovato OWNER trovato;"
$(brew --prefix libpq)/bin/pg_restore \
  -d postgres://trovato:trovato@localhost:5432/trovato \
  backups/<name>.dump
```

Backups live in `backups/` (gitignored). Existing snapshots:

| File | Contents |
|---|---|
| `backups/tutorial-part-01-20260303.dump` | DB after Tutorial Part 1 — three hand-created conferences, no importer data |
| `backups/tutorial-part-02-20260304.dump` | DB after Tutorial Part 2 — 5,492 conferences imported, taxonomy seeded, ritrovo gather queries live |

## TOOLS.md — Environment Discovery File

`TOOLS.md` is an environment-specific discovery file. It records how to do things in this particular installation -- paths, URLs, working commands, database connection strings, admin credentials, API endpoints. It is NOT checked into git (add to `.gitignore`).

**Build it as you go.** When you figure out how to do something (start the server, connect to the database, hit an API endpoint, access the admin UI), record it in `TOOLS.md` under a descriptive heading. Include the exact command or URL that worked, not a generic template.

**Check it first.** Before trying to figure out how to do something, check `TOOLS.md`. If it's already there, use what's recorded. If it's not, figure it out, then record it.

**Sections to expect:** Prerequisites (versions, paths), Server (start/stop/health check), Database (connection string, useful queries), Admin UI (base URL, login), API (endpoints discovered so far), Config (import/export commands), Build (cargo commands, test commands), Plugins (compile, install, debug).

## Working Through the Tutorial

The tutorial in `docs/tutorial/` has companion agent recipes in `docs/tutorial/recipes/`. When working through any tutorial part:

1. **Run sync check first.** `bash docs/tutorial/recipes/sync-check.sh`. If any recipe is out of sync with its tutorial file, diff them, update the recipe to match, update the sync hash, and commit before proceeding.
2. **Follow the recipe, not the tutorial.** The recipe has the same steps but is written for you -- it tells you what tools to use, how to verify each step, and where to look things up.
3. **Record discoveries in TOOLS.md.** Every time you figure out a path, URL, command, or workaround, add it to `TOOLS.md` so you (or a future session) don't have to rediscover it.
4. **If a recipe step is wrong or unclear, fix it.** Update the recipe, re-hash, commit. The recipes are meant to improve over time.

## Before Committing Checklist

1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --all`
