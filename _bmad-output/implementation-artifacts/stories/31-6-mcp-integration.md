# Story 31.6: MCP Server Integration

Status: done

## Story

As a developer using external AI tools,
I want to connect them to my Trovato site via MCP (Model Context Protocol),
so that I can interact with site content from Claude Desktop, Cursor, or VS Code.

## Acceptance Criteria

1. **AC1: Separate MCP Binary** — A new binary target `trovato-mcp` in a `crates/mcp-server` crate links against the `trovato_kernel` library. Runs as a STDIO MCP server (the standard transport for Claude Desktop, Cursor, VS Code). Connects to the same PostgreSQL and Redis as the kernel. Initialized via the same `AppState` machinery.

2. **AC2: MCP Protocol Compliance** — The server implements JSON-RPC 2.0 per the MCP specification (2025-11-25). Supports `initialize` handshake with server capabilities, `tools/list`, `tools/call`, `resources/list`, `resources/read`. Tool descriptions include full JSON Schema parameter definitions. Resource URIs follow `trovato://` scheme.

3. **AC3: Content Tools** — MCP tools for content operations:
   - `list_items` — query items by content type, status, author, with pagination (wraps existing item list logic)
   - `get_item` — fetch a single item by ID with all fields (wraps `Item::load()`)
   - `create_item` — create a new item (wraps `Item::create()`)
   - `update_item` — update an existing item (wraps `Item::update()`)
   - `delete_item` — delete an item (wraps `Item::delete()`)
   - `search` — full-text search with pagination (wraps `SearchService::search()`)

4. **AC4: Schema & Category Tools** — MCP tools for structure:
   - `list_content_types` — return all content type names and field definitions
   - `list_categories` — list all category vocabularies
   - `list_tags` — list tags in a vocabulary, with hierarchy info
   - `run_gather` — execute a named Gather query definition

5. **AC5: Resources** — MCP resources exposing read-only context:
   - `trovato://content-types` — all content type schemas (field names, types, required)
   - `trovato://content-type/{name}` — single content type schema
   - `trovato://site-config` — public site configuration (site name, slogan, default language)
   - `trovato://recent-items` — 20 most recent published items (title, type, URL, created)

6. **AC6: Authentication** — The MCP server authenticates the connecting user via an API token passed as a CLI argument or environment variable (`TROVATO_API_TOKEN`). The token resolves to a user via the existing `ApiToken` model. All tool calls execute with that user's permissions. Invalid or missing token returns an error during `initialize`.

7. **AC7: Permission Enforcement** — Every tool call checks the resolved user's permissions before executing. Same access control as REST API: `access content` for reading, `create content` for creating, `edit content` / `edit own content` for updating, `delete content` / `delete own content` for deleting, `configure site` for admin-only operations. No special MCP-only permissions — `use ai` is NOT required.

8. **AC8: Configuration** — Connection parameters via environment variables: `DATABASE_URL`, `REDIS_URL`, `TROVATO_API_TOKEN`. No admin UI needed for v1 — the MCP server is a developer tool, configured via environment.

9. **AC9: Integration Tests** — Tests verify: (a) tool list returns expected tools; (b) `get_item` returns correct data; (c) `search` returns results; (d) `list_content_types` returns schema; (e) permission enforcement denies unauthorized operations; (f) invalid token is rejected.

## Tasks / Subtasks

- [x] Task 1: Create `crates/mcp-server` crate (AC: #1, #2)
  - [x] 1.1 Create `crates/mcp-server/Cargo.toml` with deps: `trovato-kernel`, `rmcp`, `tokio`, `serde`, `serde_json`, `anyhow`, `clap`
  - [x] 1.2 Add `crates/mcp-server` to workspace `Cargo.toml` members
  - [x] 1.3 Create `crates/mcp-server/src/main.rs` — CLI entry point, parse `--token` arg / `TROVATO_API_TOKEN` env, initialize `AppState`, start STDIO server
  - [x] 1.4 Create `crates/mcp-server/src/server.rs` — MCP server struct implementing `rmcp::ServerHandler`, declare capabilities (tools, resources)

- [x] Task 2: Implement content tools (AC: #3)
  - [x] 2.1 Create `crates/mcp-server/src/tools/mod.rs` — tool registry, dispatch by name
  - [x] 2.2 Create `crates/mcp-server/src/tools/items.rs` — `list_items`, `get_item`, `create_item`, `update_item`, `delete_item`
  - [x] 2.3 Create `crates/mcp-server/src/tools/search.rs` — `search` tool wrapping `SearchService`

- [x] Task 3: Implement schema & category tools (AC: #4)
  - [x] 3.1 Create `crates/mcp-server/src/tools/content_types.rs` — `list_content_types` using `ContentTypeRegistry`
  - [x] 3.2 Create `crates/mcp-server/src/tools/categories.rs` — `list_categories`, `list_tags` using `Category`/`Tag` models
  - [x] 3.3 Create `crates/mcp-server/src/tools/gather.rs` — `run_gather` using `GatherService`

- [x] Task 4: Implement resources (AC: #5)
  - [x] 4.1 Create `crates/mcp-server/src/resources/mod.rs` — resource registry, dispatch by URI
  - [x] 4.2 Create `crates/mcp-server/src/resources/content_types.rs` — `trovato://content-types`, `trovato://content-type/{name}`
  - [x] 4.3 Create `crates/mcp-server/src/resources/site.rs` — `trovato://site-config`, `trovato://recent-items`

- [x] Task 5: Implement authentication & permissions (AC: #6, #7)
  - [x] 5.1 Create `crates/mcp-server/src/auth.rs` — resolve API token to user via `ApiToken::find_by_token()`, load `User`
  - [x] 5.2 Add permission checking to each tool handler using `PermissionService::user_has_permission()`

- [x] Task 6: Integration tests (AC: #9)
  - [x] 6.1 Create `crates/mcp-server/tests/mcp_test.rs` — test tool execution with in-process server (no STDIO needed)
  - [x] 6.2 Test tool list completeness (16 tests total)
  - [x] 6.3 Test item CRUD via tools (get, list, create+delete, update)
  - [x] 6.4 Test permission enforcement (create denied, delete denied for unprivileged user)
  - [x] 6.5 Test resource reading (site-config, content-types, recent-items)

- [x] Task 7: Verify (AC: all)
  - [x] 7.1 `cargo fmt --all` — clean
  - [x] 7.2 `cargo clippy --all-targets -- -D warnings` — clean
  - [x] 7.3 `cargo test --all` — 0 failures

## Dev Notes

### Architecture Decision: Separate Binary, Not WASM Plugin

The design doc (D5) envisions MCP as a WASM plugin (`trovato_mcp`). This story implements it as a **separate binary crate** instead. Justification:

1. **MCP STDIO transport** requires direct stdin/stdout access — impossible from WASM sandbox
2. **JSON-RPC 2.0 protocol** requires custom framing — not HTTP, cannot use `tap_menu`
3. **Same precedent as Story 31.5** — SSE streaming required kernel-level ChatService instead of WASM plugin
4. **Clean separation** — MCP binary imports `trovato_kernel` as library, shares all services
5. **No kernel bloat** — the MCP server is a separate binary, doesn't add code to the HTTP server

The kernel crate already has `[lib]` + `[[bin]]` structure (`trovato_kernel` lib + `trovato` bin). The MCP server is a second consumer of the library.

### `rmcp` Crate (Official Rust MCP SDK)

Use the official `rmcp` crate from `github.com/modelcontextprotocol/rust-sdk`. It provides:
- STDIO transport handler (reads JSON-RPC from stdin, writes to stdout)
- `ServerHandler` trait for implementing MCP servers
- Type-safe tool/resource definitions
- Protocol compliance with MCP spec 2025-11-25

```toml
# crates/mcp-server/Cargo.toml
[dependencies]
trovato-kernel = { path = "../kernel" }
rmcp = { version = "0.1", features = ["server", "transport-io"] }
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

**Note:** Check `rmcp` latest version on crates.io before implementation. The API may differ from examples below — read the crate docs.

### Server Implementation Pattern

```rust
// crates/mcp-server/src/main.rs
use clap::Parser;
use trovato_kernel::state::AppState;

#[derive(Parser)]
struct Cli {
    /// API token for authentication (or set TROVATO_API_TOKEN env var)
    #[arg(long, env = "TROVATO_API_TOKEN")]
    token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize kernel state (DB pool, Redis, services)
    let state = AppState::new(/* config from env */).await?;

    // Resolve token to user
    let user = auth::resolve_token(&state, &cli.token).await?;

    // Create MCP server with state + user context
    let server = TrovatoMcpServer::new(state, user);

    // Run on STDIO transport
    rmcp::transport::io::serve_stdio(server).await?;

    Ok(())
}
```

### Tool Definition Pattern

Each tool needs: name, description, JSON Schema for parameters, and a handler function.

```rust
// Tools return serde_json::Value results. The MCP server serializes them.
pub async fn handle_get_item(
    state: &AppState,
    user: &User,
    params: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let id: Uuid = params["id"].as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| anyhow::anyhow!("invalid or missing 'id' parameter"))?;

    // Permission check
    let item = Item::load(state.db(), id).await?
        .ok_or_else(|| anyhow::anyhow!("item not found"))?;

    if !state.permissions().user_has_permission(user, "access content").await? {
        anyhow::bail!("permission denied: access content");
    }

    // Return item as JSON
    Ok(serde_json::to_value(&item)?)
}
```

### Tool Parameter Schemas

Every tool must declare its parameters as JSON Schema. Be precise — LLMs use these schemas to construct calls.

```rust
fn get_item_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "format": "uuid",
                "description": "The UUID of the item to retrieve"
            }
        },
        "required": ["id"]
    })
}

fn list_items_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "content_type": {
                "type": "string",
                "description": "Filter by content type name (e.g. 'article', 'conference')"
            },
            "status": {
                "type": "integer",
                "description": "Filter by status: 1=published, 0=unpublished",
                "enum": [0, 1]
            },
            "page": {
                "type": "integer",
                "description": "Page number (1-indexed)",
                "default": 1
            },
            "per_page": {
                "type": "integer",
                "description": "Items per page (max 100)",
                "default": 20
            }
        }
    })
}
```

### Resource URI Scheme

Resources use `trovato://` URIs:
- `trovato://content-types` — JSON array of all content type definitions
- `trovato://content-type/{name}` — single content type with fields
- `trovato://site-config` — `{ "site_name": "...", "slogan": "...", "language": "..." }`
- `trovato://recent-items` — JSON array of 20 most recent published items

Resource reads are permission-gated: `access content` required for item/content-type resources.

### Authentication Flow

1. User creates an API token at `/user/{id}/tokens` in the admin UI (existing feature from `api_token.rs`)
2. User provides token to MCP server: `trovato-mcp --token trv_abc123...` or `TROVATO_API_TOKEN=trv_abc123...`
3. On startup, MCP server calls `ApiToken::verify(pool, &raw_token)` to resolve token → user_id
4. Loads full `User` record via `User::find_by_id(pool, user_id)`
5. All subsequent tool/resource calls use this user's identity and permissions
6. If token is invalid/expired, exit with error before STDIO loop starts

**Important:** The `ApiToken::verify()` method hashes the raw token with SHA-256 and looks up by hash (tokens are never stored in plaintext). The method signature is roughly:
```rust
pub async fn verify(pool: &PgPool, raw_token: &str) -> Result<Option<ApiToken>>
```

### Existing Services to Reuse

| Service | Access Pattern | Used For |
|---------|---------------|----------|
| `Item::load/create/update/delete` | `state.db()` | Item CRUD tools |
| `SearchService::search()` | `state.search()` | Search tool |
| `ContentTypeRegistry` | `state.content_types()` | Content type listing/schema |
| `Category::list/find` | `state.db()` | Category tools |
| `Tag::list_by_vocabulary` | `state.db()` | Tag listing |
| `GatherService::execute()` | `state.gather()` | Gather query tool |
| `PermissionService` | `state.permissions()` | Permission checks |
| `SiteConfig::get()` | `state.db()` | Site config resource |
| `ApiToken::verify()` | `state.db()` | Token auth |
| `User::find_by_id()` | `state.db()` | User loading |

### AppState Initialization

The `AppState::new()` in the kernel `main.rs` does full initialization (DB pool, Redis, migrations, plugin loading, service creation). The MCP server needs the same initialization but WITHOUT:
- Starting the HTTP listener
- Running the Axum router
- Starting background tasks (cron, queue workers)

Check how `AppState` is constructed. If it's tightly coupled to HTTP concerns, you may need to extract a `AppState::new_headless()` or similar that skips HTTP-specific setup. Alternatively, if the existing `AppState::new()` is clean, just call it directly.

**Key:** The kernel crate exposes `trovato_kernel::state::AppState` via its `lib.rs`. Verify that `AppState::new()` (or equivalent) is accessible from the library target. If it's only in `main.rs`, refactor the initialization into a function in `state.rs` or `lib.rs`.

### `lib.rs` Exports

Check what `crates/kernel/src/lib.rs` re-exports. The MCP server needs access to:
- `state::AppState`
- `models::{User, Item, Category, Tag, SiteConfig, ApiToken}`
- `services::{SearchService, GatherService, PermissionService}`
- `content::type_registry::ContentTypeRegistry`

If these aren't re-exported, add `pub mod` declarations as needed.

### Permission Mapping for Tools

| Tool | Required Permission |
|------|-------------------|
| `list_items` | `access content` |
| `get_item` | `access content` |
| `create_item` | `create content` (+ content type permission if applicable) |
| `update_item` | `edit content` or `edit own content` (for own items) |
| `delete_item` | `delete content` or `delete own content` (for own items) |
| `search` | `access content` |
| `list_content_types` | `access content` |
| `list_categories` | `access content` |
| `list_tags` | `access content` |
| `run_gather` | `access content` (gather results filtered by access) |

Admin users bypass permission checks (existing `user.is_admin` pattern).

### Gather Tool Implementation

The `run_gather` tool is powerful — it lets external AI tools run pre-defined queries:

```rust
pub async fn handle_run_gather(
    state: &AppState,
    user: &User,
    params: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let query_name = params["name"].as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'name' parameter"))?;

    // Load gather definition by name
    let definition = state.gather().load_by_name(query_name).await?
        .ok_or_else(|| anyhow::anyhow!("gather query not found: {}", query_name))?;

    // Execute with user context for access filtering
    let results = state.gather().execute(&definition, /* stage, page, filters */).await?;

    Ok(serde_json::to_value(&results)?)
}
```

### Testing Strategy

The MCP server can be tested without STDIO by calling the handler methods directly:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // Use kernel's test infrastructure
    use trovato_kernel::tests::common::{shared_app, run_test, SHARED_RT};

    #[test]
    fn test_list_content_types() {
        run_test(async {
            let app = shared_app().await;
            let admin = /* create admin user */;

            let result = handle_list_content_types(app.state(), &admin, json!({})).await;
            assert!(result.is_ok());
            let types = result.unwrap();
            assert!(types.as_array().unwrap().len() > 0);
        });
    }
}
```

**Note:** The MCP server tests live in `crates/mcp-server/tests/` and use the kernel's test infrastructure. This requires the kernel's test helpers to be accessible. Check if `crates/kernel/tests/common/mod.rs` can be imported or if shared test utilities need to be extracted to a `test-utils` crate.

**Alternative:** If importing kernel test infra is complex, write tests as unit tests within `crates/mcp-server/src/` using a test database directly, or use `cargo test -p mcp-server` with integration test fixtures.

### Constraints and Pitfalls

1. **Do NOT add MCP routes to the HTTP kernel** — MCP is a separate binary/process, not an HTTP endpoint. This keeps the kernel minimal per CLAUDE.md rules.
2. **Do NOT create a WASM plugin** — MCP requires STDIO access and custom JSON-RPC framing, impossible from WASM sandbox.
3. **Do NOT require `use ai` permission** — MCP is about content access, not AI operations. The MCP client (Claude, Cursor) has its own AI; Trovato just provides content.
4. **Do NOT expose API keys or secrets** via MCP resources — only public site configuration.
5. **Do NOT allow MCP to bypass stage visibility** — items should respect the same stage rules as the REST API. Default to LIVE stage unless the user has stage permissions.
6. **`AppState::new()` may need refactoring** — if it's tightly coupled to HTTP server setup, extract a shared initialization path.
7. **`rmcp` API may differ from examples** — the crate is relatively new. Read the actual crate documentation before implementing. If the API has changed significantly, adapt the patterns.
8. **Workspace member ordering** — add `crates/mcp-server` to `[workspace] members` in the root `Cargo.toml`.
9. **`trovato_kernel` lib exports** — verify all needed types are accessible from the library. Add `pub mod` re-exports if needed.
10. **Test infrastructure sharing** — kernel test helpers (`shared_app`, `run_test`, `SHARED_RT`) are in `crates/kernel/tests/common/mod.rs` which is test-only code. MCP server tests may need their own test infrastructure or a shared test-utils crate.

### Project Structure Notes

```
crates/
├── kernel/                    (existing)
│   ├── src/
│   │   ├── lib.rs            (library — all services, models, routes)
│   │   └── main.rs           (HTTP binary)
│   └── tests/                (integration tests)
├── plugin-sdk/                (existing)
├── mcp-server/                (NEW)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs           (CLI entry + STDIO transport)
│   │   ├── server.rs         (MCP ServerHandler impl)
│   │   ├── auth.rs           (token → user resolution)
│   │   ├── tools/
│   │   │   ├── mod.rs        (tool registry + dispatch)
│   │   │   ├── items.rs      (CRUD tools)
│   │   │   ├── search.rs     (search tool)
│   │   │   ├── content_types.rs (schema tools)
│   │   │   ├── categories.rs (taxonomy tools)
│   │   │   └── gather.rs     (gather query tool)
│   │   └── resources/
│   │       ├── mod.rs        (resource registry + dispatch)
│   │       ├── content_types.rs (content type schema resources)
│   │       └── site.rs       (site config + recent items)
│   └── tests/
│       └── mcp_test.rs       (integration tests)
```

### Claude Desktop Configuration

After building, users configure Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "trovato": {
      "command": "/path/to/trovato-mcp",
      "args": ["--token", "trv_abc123..."],
      "env": {
        "DATABASE_URL": "postgres://...",
        "REDIS_URL": "redis://..."
      }
    }
  }
}
```

Or with env var: set `TROVATO_API_TOKEN` in `env` block.

### References

- [Source: docs/design/ai-integration.md#D5 — MCP as a plugin]
- [Source: docs/ritrovo/epic-03.md#Story 31.10 — MCP Server Plugin acceptance criteria]
- [Source: docs/ritrovo/epic-03.md#Step 5 — MCP Server narrative]
- [Source: crates/kernel/src/middleware/api_token.rs — Bearer token auth middleware]
- [Source: crates/kernel/src/models/api_token.rs — ApiToken model, verify(), hash]
- [Source: crates/kernel/src/state.rs — AppState, service accessors]
- [Source: crates/kernel/src/lib.rs — Library target re-exports]
- [Source: crates/kernel/Cargo.toml — lib + bin dual target structure]
- [Source: MCP Specification 2025-11-25 — https://modelcontextprotocol.io/specification/2025-11-25]
- [Source: rmcp crate — https://github.com/modelcontextprotocol/rust-sdk]

### Previous Story Intelligence

**From Story 31.5 (Chatbot):**
- Kernel service pattern (ChatService) — separate from WASM plugins when protocol handling required
- `SiteConfig::get/set` for config storage — reuse for MCP config if needed
- `SearchService::search()` API: `search(query, stage_ids, user_id, limit, offset)` returns `SearchResults { results, total }`
- `DashMap` rate limiter pattern — may want per-tool rate limiting in future
- `AI_CHAT_LOCK` mutex pattern for test serialization of shared config
- `tower_sessions::Session` is Clone (Arc-backed) — not relevant for MCP (no sessions)

**From Story 31.4 (AI Permissions):**
- `PermissionService::user_has_permission(&user, "permission_name")` returns `Result<bool>`
- Admin users (`user.is_admin`) bypass permission checks
- `AVAILABLE_PERMISSIONS` constant in kernel declares all permission strings

**From Story 31.1 (AI Provider Registry):**
- `AppState` holds services as `Arc<ServiceType>` with getter methods
- `AppStateInner` is the actual struct; `AppState` is `Arc<AppStateInner>` with `Deref`
- Service initialization follows builder pattern in `main.rs`

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6

### Debug Log References
- Session context compacted once (large file set + rmcp research)

### Completion Notes List
- Code review fixed 6 issues: removed unjustified `#![allow(dead_code)]`, added `access content` check to `update_item`/`delete_item` before item lookup, replaced 14 `unwrap_or_default()` with proper `to_json()` error helper, added invalid token rejection test (AC9f), added single content-type resource template test (AC5)
- Used `rmcp` v0.16.0 (official MCP Rust SDK). Key API differences from story notes: `#[tool_router]` + `#[tool]` macros for tools; `ServerHandler` trait for resources; `ServiceExt::serve()` + `rmcp::transport::stdio()` for STDIO transport; `schemars` v1.0 for JSON Schema derivation; `Parameters<T>` wrapper for typed tool params
- `AppState` doesn't implement `Debug` — used manual `Debug` impl with `finish_non_exhaustive()`
- `ApiToken` not re-exported from `trovato_kernel::models` — imported via `trovato_kernel::models::api_token::ApiToken`
- `RawResource`/`RawResourceTemplate` in rmcp 0.16 have no builder methods or Default impl — direct struct construction required
- Permission test: authenticated role grants "access content" by default, so permission denial tests use `create_item`/`delete_item` instead of `list_items`
- Item struct has `#[serde(rename = "type")]` on `item_type` field — tests use `json["type"]`

### File List
- `crates/mcp-server/Cargo.toml` — NEW: crate definition with lib+bin targets
- `crates/mcp-server/src/lib.rs` — NEW: library entry exposing modules
- `crates/mcp-server/src/main.rs` — NEW: CLI entry point with STDIO transport
- `crates/mcp-server/src/server.rs` — NEW: TrovatoMcpServer with 10 tools via `#[tool_router]`
- `crates/mcp-server/src/auth.rs` — NEW: token resolution + permission checking
- `crates/mcp-server/src/tools/mod.rs` — NEW: tool module registry
- `crates/mcp-server/src/tools/items.rs` — NEW: list/get/create/update/delete tools
- `crates/mcp-server/src/tools/search.rs` — NEW: full-text search tool
- `crates/mcp-server/src/tools/content_types.rs` — NEW: content type schema tool
- `crates/mcp-server/src/tools/categories.rs` — NEW: category/tag tools
- `crates/mcp-server/src/tools/gather.rs` — NEW: gather query tool
- `crates/mcp-server/src/resources/mod.rs` — NEW: resource registry + dispatch
- `crates/mcp-server/src/resources/content_types.rs` — NEW: content type resources
- `crates/mcp-server/src/resources/site.rs` — NEW: site config + recent items resources
- `crates/mcp-server/tests/common/mod.rs` — NEW: test infrastructure (SHARED_RT, TestContext)
- `crates/mcp-server/tests/mcp_test.rs` — NEW: 16 integration tests
- `Cargo.toml` (workspace root) — MODIFIED: added `crates/mcp-server` to members + default-members
- `crates/kernel/src/permissions.rs` — MODIFIED: made `load_user_permissions` public for MCP auth
