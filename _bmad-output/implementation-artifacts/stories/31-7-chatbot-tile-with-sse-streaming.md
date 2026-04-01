# Story 31.7: Chatbot Tile with SSE Streaming

Status: done

## Story

As a **site visitor**,
I want to ask questions about site content in a chat interface,
so that I can get direct answers without navigating the site manually.

## Acceptance Criteria

1. **AC1: Chat Tile Type** — A new `chat` tile type exists in the tile system. When placed in any region via the tile admin UI at `/admin/structure/tiles`, it renders a chat widget with input field and message area. The tile template is vanilla JS (no external framework).

2. **AC2: SSE Chat Endpoint** — `POST /api/v1/chat` accepts `{ "message": "..." }` and returns an SSE stream (`text/event-stream`). Each token arrives as a `data:` event with JSON `{"type":"token","text":"..."}`. Stream ends with `{"type":"done","usage":{...}}`. Requires authenticated session with `use ai` + `use ai chat` permissions.

3. **AC3: RAG Context Injection** — Before sending to the AI provider, the service queries `SearchService::search()` with the user's message. Top N results (configurable, default 5) are injected into the system prompt as context with title, snippet, and URL. Search respects existing access control.

4. **AC4: System Prompt Configuration** — Admin UI at `/admin/system/ai-chat` allows configuring: system prompt text (with `{site_name}` template variable), RAG toggle, RAG max results, RAG minimum score threshold, max conversation history turns, rate limit per hour, max tokens, temperature. Stored in `site_config` key `"ai_chat_config"`.

5. **AC5: Conversation History** — Conversation turns (role + content + timestamp) stored in Redis session under key `"chat_history"`. Capped at `max_history_turns` (oldest removed first). Cleared on session end. History sent to AI provider as prior messages for multi-turn context.

6. **AC6: Provider Streaming** — Requests sent with `stream: true`. OpenAI-compatible responses parsed for `choices[0].delta.content` tokens. Anthropic responses parsed for `content_block_delta` events. Token usage recorded to `ai_usage_log` after stream completes. Budget checked before stream starts.

7. **AC7: Client-Side Chat Widget** — Vanilla JS handles: sending messages via `fetch()`, consuming SSE via `ReadableStream` (not EventSource — POST required), rendering tokens incrementally, auto-scrolling, conversation history display, loading state, CSRF token from meta tag.

8. **AC8: Rate Limiting** — Per-user rate limiting keyed by user_id (DashMap-based, in-memory). Configurable limits in `ai_chat_config`. Default: 20 requests/hour. Returns 429 with error message when exceeded. Stale entries evicted every 60 seconds.

9. **AC9: Admin Sidebar** — "AI Chat" link appears under the System section in `page--admin.html`. Protected by `configure ai` permission.

10. **AC10: Integration Tests** — Tests verify: (a) authenticated user gets SSE content-type on POST `/api/v1/chat`; (b) user without permission gets 403; (c) unauthenticated user gets 403; (d) admin can save/load chat config; (e) empty message returns 400; (f) rate limiter state management.

## Dev Notes

### Key Implementation Details

- `ChatService` (1115 lines) in `crates/kernel/src/services/ai_chat.rs` — config, RAG context assembly, message building, SSE stream parsing for both OpenAI and Anthropic protocols
- `api_chat.rs` (481 lines) in `crates/kernel/src/routes/api_chat.rs` — SSE endpoint, rate limiter, permission checks, session history management
- `admin_ai_chat.rs` (135 lines) in `crates/kernel/src/routes/admin_ai_chat.rs` — Admin config GET/POST
- Chat is a kernel service (not WASM plugin) because SSE streaming requires direct HTTP response stream access
- Does NOT use the WASM `ai_request()` host function — calls providers directly via `AiProviderService`
- Session cloned into `async_stream` closure for server-side token accumulation
- `reqwest::Client` timeout overridden to 120s per-request for streaming (default is 10s)
- Uses `fetch()` + `ReadableStream` on client side (not `EventSource` which only supports GET)

### Architecture

Request flow: Client POST -> auth + permission + rate limit -> ChatService::search_for_context() (PostgreSQL FTS) -> ChatService::build_messages() (system + RAG + history + user) -> ChatService::execute_streaming() (resolve provider, HTTP with stream:true) -> SSE response -> record_usage on completion

### Dependencies Added

- `tokio-stream` and `async-stream` to `crates/kernel/Cargo.toml`
- `futures-core` to `crates/kernel/Cargo.toml`
- `stream` feature added to workspace reqwest

### Files

**Created:**
- `crates/kernel/src/services/ai_chat.rs` (1115 lines)
- `crates/kernel/src/routes/api_chat.rs` (481 lines)
- `crates/kernel/src/routes/admin_ai_chat.rs` (135 lines)
- `templates/admin/ai-chat.html`

**Modified:**
- `Cargo.toml` (workspace) — stream feature for reqwest
- `crates/kernel/Cargo.toml` — tokio-stream, async-stream, futures-core
- `crates/kernel/src/main.rs` — Wire ChatService into AppState
- `crates/kernel/src/state.rs` — ai_chat field + getter
- `crates/kernel/src/services/mod.rs` — `pub mod ai_chat;`
- `crates/kernel/src/routes/mod.rs` — `pub mod api_chat;` + `pub mod admin_ai_chat;`
- `crates/kernel/src/routes/admin.rs` — Merge admin_ai_chat router
- `crates/kernel/src/services/tile.rs` — Add `chat` tile type with inline JS widget
- `crates/kernel/src/routes/tile_admin.rs` — Add `chat` to tile type options
- `templates/admin/tile-form.html` — Add `chat` option to dropdown
- `templates/page--admin.html` — Add "AI Chat" sidebar link
- `templates/base.html` — Add CSRF meta tag for authenticated users
- `crates/kernel/tests/integration_test.rs` — AI_CHAT_LOCK + 6 integration tests

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Server-side token accumulation replaced client-side `save_history` POST
- `ChatTurn.role` is `ChatRole` enum with `serde(rename_all = "lowercase")`
- Three rounds of adversarial review completed with all findings resolved
- Budget check occurs before stream start to avoid consuming resources before confirming budget

### File List

- `crates/kernel/src/services/ai_chat.rs` (1115 lines)
- `crates/kernel/src/routes/api_chat.rs` (481 lines)
- `crates/kernel/src/routes/admin_ai_chat.rs` (135 lines)
- `templates/admin/ai-chat.html`
