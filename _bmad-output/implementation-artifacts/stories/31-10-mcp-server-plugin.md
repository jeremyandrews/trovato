# Story 31.10: MCP Server Plugin

Status: done

## Story

As a **developer using external AI tools**,
I want to connect them to my Trovato site via MCP (Model Context Protocol),
so that I can interact with site content from Claude Desktop, Cursor, or VS Code.

## Acceptance Criteria

1. **AC1: Separate MCP Binary** — A new binary target `trovato-mcp` in `crates/mcp-server/` links against `trovato_kernel` as a library. Runs as a STDIO MCP server (standard transport for Claude Desktop, Cursor, VS Code). Connects to the same PostgreSQL and Redis as the kernel via shared `AppState`.

2. **AC2: MCP Protocol Compliance** — Implements JSON-RPC 2.0 per MCP specification (2025-11-25) via the `rmcp` crate (v0.16). Supports `initialize` handshake, `tools/list`, `tools/call`, `resources/list`, `resources/read`. Tool descriptions include full JSON Schema parameter definitions via `schemars`.

3. **AC3: Content Tools** — Ten MCP tools for content operations:
   - `list_items` — query items by content type, status, author, with pagination
   - `get_item` — fetch a single item by ID with all fields
   - `create_item` — create a new item with content type, title, status, fields
   - `update_item` — update an existing item with optional title, status, fields, log message
   - `delete_item` — delete an item by ID
   - `search` — full-text search with query, limit, offset
   - `list_content_types` — return all content type names
   - `list_categories` — list all category vocabularies with tag counts
   - `list_tags` — list tags in a category with hierarchy info
   - `run_gather` — execute a named Gather query definition with optional filters

4. **AC4: Resources** — MCP resources exposing read-only context via `trovato://` URI scheme:
   - `trovato://content-types` — all content type schemas (field names, types, required)
   - `trovato://content-type/{name}` — single content type schema (resource template)
   - `trovato://site-config` — public site configuration (site name, slogan, language)
   - `trovato://recent-items` — 20 most recent published items

5. **AC5: Authentication** — API token authentication via `TROVATO_API_TOKEN` env var, `--token` arg, or `--token-file` path. Token resolved to user via `ApiToken` model at startup. All tool calls execute with that user's permissions. Invalid token exits with error before STDIO loop.

6. **AC6: Permission Enforcement** — Every tool call checks permissions: `access content` for reading, `create content` for creating, `edit content`/`edit own content` for updating, `delete content`/`delete own content` for deleting. Admin users bypass checks. No special MCP-only permissions required.

7. **AC7: Integration Tests** — 16 integration tests in `crates/mcp-server/tests/mcp_test.rs` verify: tool list completeness, item CRUD via tools, permission enforcement, resource reading, invalid token rejection, content type schema resources.

## Dev Notes

### Architecture Decision

Implemented as a separate binary crate (not WASM plugin) because:
1. MCP STDIO transport requires direct stdin/stdout access (impossible from WASM sandbox)
2. JSON-RPC 2.0 requires custom framing (not HTTP, cannot use `tap_menu`)
3. Same precedent as Story 31.7 (ChatService in kernel for SSE streaming)
4. Clean separation: MCP binary imports `trovato_kernel` as library

### Key Implementation Details

- `crates/mcp-server/` — 2063 total lines across all source files
- `server.rs` (372 lines) — `TrovatoMcpServer` implementing `rmcp::ServerHandler` with `#[tool_router]` and `#[tool]` macros
- `auth.rs` (64 lines) — Token resolution via `ApiToken::find_by_token()` + permission loading via `build_user_context()`
- `main.rs` (161 lines) — CLI entry with clap, STDIO transport via `rmcp::ServiceExt::serve()`
- `tools/items.rs` (271 lines) — CRUD tools with permission checks and pagination
- `tools/gather.rs` (84 lines) — Gather query execution tool
- `tools/search.rs` (51 lines) — Full-text search tool
- `tools/categories.rs` (48 lines) — Category and tag listing
- `tools/content_types.rs` (23 lines) — Content type schema listing
- `resources/mod.rs` (104 lines) — Resource registry with `trovato://` URI dispatch
- `resources/site.rs` (77 lines) — Site config and recent items resources
- `resources/content_types.rs` (35 lines) — Content type schema resources
- Uses `schemars` v1.0 for JSON Schema derivation of tool parameter types
- `RawResource`/`RawResourceTemplate` constructed directly (no builder methods in rmcp 0.16)
- Test infrastructure in `tests/common/mod.rs` (174 lines) with own `SHARED_RT` and `TestContext`

### Files

**Created:**
- `crates/mcp-server/Cargo.toml`
- `crates/mcp-server/src/lib.rs`
- `crates/mcp-server/src/main.rs` (161 lines)
- `crates/mcp-server/src/server.rs` (372 lines)
- `crates/mcp-server/src/auth.rs` (64 lines)
- `crates/mcp-server/src/tools/mod.rs` (67 lines)
- `crates/mcp-server/src/tools/items.rs` (271 lines)
- `crates/mcp-server/src/tools/search.rs` (51 lines)
- `crates/mcp-server/src/tools/content_types.rs` (23 lines)
- `crates/mcp-server/src/tools/categories.rs` (48 lines)
- `crates/mcp-server/src/tools/gather.rs` (84 lines)
- `crates/mcp-server/src/resources/mod.rs` (104 lines)
- `crates/mcp-server/src/resources/content_types.rs` (35 lines)
- `crates/mcp-server/src/resources/site.rs` (77 lines)
- `crates/mcp-server/tests/common/mod.rs` (174 lines)
- `crates/mcp-server/tests/mcp_test.rs` (523 lines, 16 tests)

**Modified:**
- `Cargo.toml` (workspace) — Added `crates/mcp-server` to members + default-members
- `crates/kernel/src/permissions.rs` — Made `load_user_permissions` public for MCP auth

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Used `rmcp` v0.16.0 (official MCP Rust SDK from modelcontextprotocol/rust-sdk)
- `#[tool_router]` + `#[tool]` macros for tool registration, `Parameters<T>` wrapper for typed params
- `AppState` doesn't implement `Debug` — used manual `Debug` impl with `finish_non_exhaustive()`
- `ApiToken` imported via `trovato_kernel::models::api_token::ApiToken` (not re-exported from models)
- Permission denial tests use `create_item`/`delete_item` (authenticated role has `access content` by default)
- Item struct `item_type` serializes as `"type"` due to `#[serde(rename = "type")]`

### File List

- `crates/mcp-server/` (2063 total lines, 16 source files + 2 test files)
