# Claude Code Guidelines

## Commit Messages

- Do NOT include "Co-Authored-By: Claude" or similar attribution lines
- Do NOT advertise or mention Claude/Anthropic in commit messages
- Keep commit messages focused on the technical changes only

## Code Deduplication Rules

- `html_escape` — use `crate::routes::helpers::html_escape`. Never create local copies.
- `SESSION_USER_ID` — use `crate::routes::auth::SESSION_USER_ID`. Never redefine.
- `is_valid_machine_name` — use `crate::routes::helpers::is_valid_machine_name`.
- `render_error` / `render_not_found` — use `crate::routes::helpers::{render_error, render_not_found}`.
- CSRF verification — use `crate::routes::helpers::require_csrf`. Never inline the pattern.
- New admin route handlers go in the appropriate `admin_*.rs` domain module, not `admin.rs`.
- New admin list/form templates should use macros from `templates/admin/macros/`.
