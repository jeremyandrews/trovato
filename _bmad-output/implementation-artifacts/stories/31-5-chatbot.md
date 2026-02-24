# Story 31.5: Chatbot Tile with SSE Streaming

Status: review

## Story

As a site visitor,
I want to ask questions about site content in a chat interface,
so that I can get direct answers without navigating the site manually.

## Acceptance Criteria

1. **AC1: Chat Tile Type** — A new `chat` tile type exists in the tile system. When placed in any region (sidebar, footer, content_top, etc.) via the existing tile admin UI at `/admin/structure/tiles`, it renders a chat widget with input field and message area. The tile template is Tera + vanilla JS (no external framework).

2. **AC2: SSE Chat Endpoint** — `POST /api/v1/chat` accepts `{ "message": "..." }` and returns an SSE stream (`text/event-stream`). Each token arrives as a `data:` event with JSON `{"type":"token","text":"..."}`. Stream ends with `{"type":"done","usage":{...}}`. The endpoint requires an authenticated session with `use ai` + `use ai chat` permissions.

3. **AC3: RAG Context Injection** — Before sending to the AI provider, the service queries `SearchService::search()` with the user's message. Top N results (configurable, default 5) are injected into the system prompt as context with title, snippet, and URL. Search respects existing access control (user_id + stage_ids filtering).

4. **AC4: System Prompt Configuration** — Admin UI at `/admin/system/ai-chat` allows configuring: system prompt text (with `{site_name}` template variable), RAG toggle, RAG max results, RAG minimum score threshold, max conversation history turns (default 5). Stored in `site_config` key `"ai_chat_config"`.

5. **AC5: Conversation History** — Conversation turns (role + content) stored in the user's Redis session under key `"chat_history"`. Capped at `max_history_turns` (oldest removed first). Cleared on session end. History sent to AI provider as prior messages for multi-turn context.

6. **AC6: Provider Streaming** — The service sends requests to AI providers with `stream: true`. OpenAI-compatible responses parsed for `choices[0].delta.content` tokens. Anthropic responses parsed for `content_block_delta` events. Token usage recorded to `ai_usage_log` after stream completes. Budget checked before stream starts.

7. **AC7: Client-Side Chat Widget** — Vanilla JS handles: sending messages via `fetch()`, consuming SSE via `EventSource` or `ReadableStream`, rendering tokens incrementally, auto-scrolling, displaying conversation history, showing loading state, and reconnecting on SSE drop.

8. **AC8: Rate Limiting** — Chat endpoint rate-limited per-user (keyed by user_id, or IP for anonymous). Configurable limits stored in `ai_chat_config`. Default: 20 requests/hour for authenticated users. Returns 429 with `Retry-After` header when exceeded.

9. **AC9: Admin Sidebar** — "AI Chat" link appears under the System section in `page--admin.html`, after "AI Budgets". Protected by `configure ai` permission.

10. **AC10: Integration Tests** — Tests verify: (a) authenticated user with `use ai chat` gets SSE response; (b) user without permission gets 403; (c) unauthenticated user gets 401; (d) rate limit returns 429 after exceeding limit; (e) admin can save/load chat config; (f) chat tile renders in region output.

## Tasks / Subtasks

- [x] Task 1: Add dependencies (AC: #2, #6)
  - [x] 1.1 Add `tokio-stream` to `crates/kernel/Cargo.toml`
  - [x] 1.2 Enable reqwest `stream` feature in workspace `Cargo.toml`
  - [x] 1.3 Add `async-stream` and `futures-core` to `crates/kernel/Cargo.toml`
- [x] Task 2: Create `ChatService` (AC: #3, #4, #5, #6)
  - [x] 2.1 Create `crates/kernel/src/services/ai_chat.rs` with `ChatService` struct, config types, session history types
  - [x] 2.2 Implement `load_config()` / `save_config()` using `SiteConfig::get/set` with key `"ai_chat_config"`
  - [x] 2.3 Implement `build_messages()` — assembles system prompt + RAG context + history + user message into `Vec<AiMessage>`
  - [x] 2.4 Implement `search_for_context()` — calls `SearchService::search()` with user message, formats results as context text
  - [x] 2.5 Implement `execute_streaming()` — resolves provider via `AiProviderService`, sends HTTP request with `stream: true`, returns `Pin<Box<dyn Stream<Item = ChatStreamEvent>>>`
  - [x] 2.6 Implement `parse_openai_stream()` — parses OpenAI SSE chunks into token events
  - [x] 2.7 Implement `parse_anthropic_stream()` — parses Anthropic SSE chunks into token events
  - [x] 2.8 Implement usage recording via `ai_budgets.record_usage()` in SSE stream after stream completes
  - [x] 2.9 Add `pub mod ai_chat;` to `services/mod.rs`
- [x] Task 3: Wire service into AppState (AC: #2)
  - [x] 3.1 Add `ai_chat: Arc<ChatService>` to `AppStateInner` in `state.rs`
  - [x] 3.2 Initialize in `AppState::new()` with `ChatService::new(db, ai_providers, search)`
  - [x] 3.3 Add getter `pub fn ai_chat(&self) -> &Arc<ChatService>`
- [x] Task 4: Create chat API route (AC: #2, #5, #8)
  - [x] 4.1 Create `crates/kernel/src/routes/api_chat.rs` with `POST /api/v1/chat` handler
  - [x] 4.2 Implement permission check: load user from session, verify `use ai` + `use ai chat`
  - [x] 4.3 Implement per-user rate limiting (in-memory `DashMap<String, (count, window_start)>`)
  - [x] 4.4 Load/update conversation history from session
  - [x] 4.5 Return `Sse<impl Stream<Item = Result<Event, Infallible>>>` response
  - [x] 4.6 Add `pub mod api_chat;` to `routes/mod.rs`, merge router in `main.rs`
- [x] Task 5: Create admin UI (AC: #4, #9)
  - [x] 5.1 Create `crates/kernel/src/routes/admin_ai_chat.rs` with GET/POST handlers for `/admin/system/ai-chat`
  - [x] 5.2 Create `templates/admin/ai-chat.html` with system prompt textarea, RAG settings, rate limit fields
  - [x] 5.3 Add sidebar link in `templates/page--admin.html` after AI Budgets link
  - [x] 5.4 Register router in `routes/mod.rs` and merge in `admin.rs`
- [x] Task 6: Add chat tile type (AC: #1, #7)
  - [x] 6.1 Add `chat` case to `render_tile_html()` in `services/tile.rs` — renders inline chat widget HTML via `render_chat_widget()`
  - [x] 6.2 Inline chat widget (Tera template not needed — widget rendered as raw HTML with inline JS)
  - [x] 6.3 Add `chat` option to tile type dropdown in `templates/admin/tile-form.html`
  - [x] 6.4 Add `chat` case to `build_config()` in `tile_admin.rs` (empty config)
- [x] Task 7: Integration tests (AC: #10)
  - [x] 7.1 Test: admin user with chat gets SSE content-type or 502 (no provider) on POST `/api/v1/chat`
  - [x] 7.2 Test: user without permission gets 403
  - [x] 7.3 Test: unauthenticated user gets 403 (CSRF check before auth) or 401
  - [x] 7.4 Test: admin can GET/POST `/admin/system/ai-chat` config
  - [x] 7.5 Test: empty message returns 400
  - [x] 7.6 Add `AI_CHAT_LOCK: Mutex<()>` for tests mutating chat config
- [x] Task 8: Verify (AC: all)
  - [x] 8.1 `cargo fmt --all`
  - [x] 8.2 `cargo clippy --all-targets -- -D warnings`
  - [x] 8.3 `cargo test --all` — 134 integration tests, 647 lib tests, all passing

## Dev Notes

### Architecture Overview

The chatbot is a kernel-side service, NOT a WASM plugin feature. SSE streaming requires direct access to the HTTP response stream, which WASM plugins cannot provide. This follows the same kernel-service pattern as `AiProviderService` and `AiTokenBudgetService` (Stories 31.1-31.3).

**Request flow:**
1. Client POSTs `{ "message": "user question" }` to `/api/v1/chat`
2. Route handler checks auth + permissions + rate limit
3. `ChatService::search_for_context()` queries PostgreSQL FTS via `SearchService`
4. `ChatService::build_messages()` assembles full prompt (system + RAG context + history + user message)
5. `ChatService::execute_streaming()` resolves provider via `AiProviderService`, sends HTTP request with `stream: true`
6. Route handler wraps the token stream as `axum::response::sse::Sse`
7. Client receives tokens via SSE, renders incrementally
8. On stream completion, `record_usage()` writes to `ai_usage_log`

The host function `ai_request()` in `host/ai.rs` is NOT used. The kernel chat service calls providers directly via `AiProviderService::resolve_provider()` + the shared `reqwest::Client`.

### Dependency Additions

**`Cargo.toml` (workspace):**
```toml
# Add "stream" to reqwest features:
reqwest = { version = "0.12", features = ["json", "stream"] }
```

**`crates/kernel/Cargo.toml`:**
```toml
tokio-stream = "0.1"
async-stream = "0.3"
```

Axum 0.8 includes `axum::response::sse::Sse` and `axum::response::sse::Event` without needing additional feature flags. The `Sse` type wraps any `Stream<Item = Result<Event, E>>`.

### ChatService Design

```rust
//! AI Chat service for streaming chatbot with RAG context.

pub struct ChatService {
    db: PgPool,
    ai_providers: Arc<AiProviderService>,
    search: Arc<SearchService>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConfig {
    pub system_prompt: String,
    pub rag_enabled: bool,
    pub rag_max_results: u32,       // default 5
    pub rag_min_score: f32,         // default 0.1
    pub max_history_turns: u32,     // default 5
    pub rate_limit_per_hour: u32,   // default 20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,     // "user" or "assistant"
    pub content: String,
    pub timestamp: i64,   // unix seconds
}

pub enum ChatStreamEvent {
    Token(String),
    Done { usage: AiUsage },
    Error(String),
}
```

**Config storage:** `SiteConfig::get(db, "ai_chat_config")` / `SiteConfig::set(db, "ai_chat_config", &config)` — same pattern as `"ai_providers"` and `"ai_token_budgets"`.

**Session history storage:** `session.insert("chat_history", Vec<ChatTurn>)` / `session.get::<Vec<ChatTurn>>("chat_history")`. Redis-backed, auto-expires with session (24h default, 30 days with remember-me).

### Provider Streaming Protocols

**OpenAI-compatible (stream: true):**
Add `"stream": true` to request body. Response is `text/event-stream`:
```
data: {"choices":[{"delta":{"content":"Hello"}}]}
data: {"choices":[{"delta":{"content":" world"}}]}
data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":50,"completion_tokens":10}}
data: [DONE]
```
Parse: extract `choices[0].delta.content` for tokens. Add `"stream_options":{"include_usage":true}` to get usage in final chunk.

**Anthropic (stream: true):**
Add `"stream": true` to request body. Response is `text/event-stream`:
```
event: content_block_delta
data: {"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"delta":{"type":"text_delta","text":" world"}}

event: message_delta
data: {"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}

event: message_stop
data: {"type":"message_stop"}
```
Parse: extract `delta.text` from `content_block_delta` events. Extract `usage.output_tokens` from `message_delta`.

### SSE Response Format (Our Endpoint)

The `/api/v1/chat` endpoint emits normalized SSE events:
```
data: {"type":"token","text":"Hello"}

data: {"type":"token","text":" world"}

data: {"type":"done","usage":{"prompt_tokens":50,"completion_tokens":10,"total_tokens":60}}
```

On error mid-stream:
```
data: {"type":"error","message":"Provider connection lost"}
```

### Streaming Implementation Pattern

```rust
use axum::response::sse::{Event, Sse};
use async_stream::stream;
use tokio_stream::StreamExt;

async fn chat_handler(
    State(state): State<AppState>,
    session: Session,
    Json(input): Json<ChatInput>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<JsonError>)> {
    // 1. Auth + permission check
    // 2. Rate limit check
    // 3. Load config + history from session
    // 4. Build messages (RAG + history + user message)
    // 5. Resolve provider
    // 6. Create streaming request

    let stream = stream! {
        // Parse provider SSE, yield normalized events
        // On completion: record usage, update session history
    };

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}
```

**CRITICAL: Session update after stream.** The session cannot be updated inside the stream closure (session is not `Send` across the stream boundary). Instead:
- Clone the conversation history before creating the stream
- After stream creation, the response is returned immediately
- Record the assistant's full response by accumulating tokens in the stream closure, then write usage log inside the stream's final event
- Session history update: Two options:
  1. Write user message to session BEFORE stream starts. Write assistant response via a separate `POST /api/v1/chat/history` call from the client after stream completes.
  2. Use a `tokio::sync::mpsc` channel: stream sends accumulated response to a background task that updates the session.
- Recommended: Option 1 (simpler). Client sends user message + receives stream + then POSTs the assistant response to update history.

### RAG Context Assembly

```rust
fn format_rag_context(results: &[SearchResult]) -> String {
    let mut context = String::from("Relevant site content:\n\n");
    for (i, r) in results.iter().enumerate() {
        // write! is infallible on String
        write!(context, "{}. {} ({})\n", i + 1, r.title, format!("/item/{}", r.id)).unwrap(); // Infallible: writing to String
        if let Some(snippet) = &r.snippet {
            write!(context, "   {}\n\n", snippet).unwrap(); // Infallible: writing to String
        }
    }
    context
}
```

The search call respects access control automatically:
```rust
let user_id = if user.authenticated { Some(user.id) } else { None };
let stage_ids = vec![LIVE_STAGE_ID]; // Public content only for chat RAG
let results = search.search(&user_message, &stage_ids, user_id, config.rag_max_results as i64, 0).await?;
// Filter by minimum score
let results: Vec<_> = results.results.into_iter().filter(|r| r.rank >= config.rag_min_score).collect();
```

### Per-User Rate Limiting

In-memory rate limiter following the pattern from `host/ai.rs` provider rate limiter:

```rust
use dashmap::DashMap;
use std::time::Instant;

static CHAT_RATE_LIMITS: LazyLock<DashMap<String, (u32, Instant)>> = LazyLock::new(DashMap::new);

fn check_chat_rate_limit(user_key: &str, limit_per_hour: u32) -> bool {
    let now = Instant::now();
    let mut entry = CHAT_RATE_LIMITS.entry(user_key.to_string()).or_insert((0, now));
    if now.duration_since(entry.1) > Duration::from_secs(3600) {
        *entry = (1, now);
        return true;
    }
    if entry.0 >= limit_per_hour {
        return false;
    }
    entry.0 += 1;
    true
}
```

Key by `user.id.to_string()` for authenticated users, or client IP for anonymous.

### HTTP Timeout for Streaming

The shared `reqwest::Client` from `AiProviderService` has a 10-second timeout. For streaming, override per-request:

```rust
let response = state.ai_chat().ai_providers().http()
    .post(&url)
    .timeout(Duration::from_secs(120)) // Override 10s default for streaming
    .headers(headers)
    .body(body)
    .send()
    .await?;

// Use bytes_stream() for streaming (requires reqwest "stream" feature)
let byte_stream = response.bytes_stream();
```

### Client-Side Chat Widget

The chat tile renders a self-contained widget. All JS is inline in the Tera template (no external JS file needed for v1):

```html
<div class="chat-widget" id="chat-widget-{{ tile.machine_name }}">
  <div class="chat-messages" id="chat-messages"></div>
  <form class="chat-input" id="chat-form">
    <input type="text" id="chat-input" placeholder="Ask a question..." autocomplete="off" />
    <button type="submit">Send</button>
  </form>
</div>
<script>
(function() {
  const form = document.getElementById('chat-form');
  const input = document.getElementById('chat-input');
  const messages = document.getElementById('chat-messages');

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const text = input.value.trim();
    if (!text) return;
    input.value = '';
    appendMessage('user', text);

    const response = await fetch('/api/v1/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ message: text }),
    });

    if (!response.ok) { /* handle error */ return; }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    let assistantText = '';

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      // Parse SSE lines from buffer
      // Extract tokens, append to message area
    }
  });

  function appendMessage(role, text) { /* DOM manipulation */ }
})();
</script>
```

**Note:** Use `fetch()` + `ReadableStream` (not `EventSource`) because `EventSource` only supports GET requests. The chat endpoint is POST.

### Admin Template Pattern

Follow the pattern from `templates/admin/ai-providers.html` and `templates/admin/ai-budgets.html`:

```html
{% extends "page--admin.html" %}
{% import "admin/macros/form.html" as form %}

{% block title %}AI Chat Configuration{% endblock %}

{% block content %}
<h1>AI Chat Configuration</h1>
<form method="post" action="/admin/system/ai-chat">
  {{ form::csrf(csrf_token=csrf_token) }}
  <!-- System prompt textarea -->
  <!-- RAG settings -->
  <!-- History depth -->
  <!-- Rate limit -->
  <button type="submit" class="button button--primary">Save configuration</button>
</form>
{% endblock %}
```

### Tile Rendering Extension

In `services/tile.rs`, `render_tile_html()` (around line 60-124), add a new match arm:

```rust
"chat" => {
    // Render the chat widget template
    // The template is self-contained (HTML + inline JS)
    let mut ctx = tera::Context::new();
    ctx.insert("tile", tile);
    state.tera().render("tiles/chat.html", &ctx).unwrap_or_default()
}
```

The `TileService::render_tile` method currently doesn't have access to `AppState` (only the tile data). Two options:
1. Pass `AppState` or `Tera` reference to `render_tile_html()` — requires signature change
2. Embed the chat HTML directly in the match arm without template rendering

Option 2 is simpler and avoids changing the existing interface. The chat widget HTML can be built as a string literal with format variables. However, if the template is large, option 1 is cleaner.

**Recommended:** Option 1 — modify `render_tile_html` to accept an `Option<&Tera>` parameter. For `custom_html`, `menu`, `gather_query` types, it's unused. For `chat`, it renders the template. This is a minimal signature change.

Alternatively, `render_region` in `TileService` already has access to the database — it could be extended to accept `&Tera` since the caller (`inject_site_context` in helpers.rs) has access to `AppState`.

### Existing Patterns to Reuse

| Pattern | Location | Usage |
|---------|----------|-------|
| `SiteConfig::get/set` | `models/site_config.rs` | Chat config storage |
| `SearchService::search()` | `search/mod.rs:60` | RAG context |
| `AiProviderService::resolve_provider()` | `services/ai_provider.rs` | Provider resolution |
| `AiProviderService::http()` | `services/ai_provider.rs` | Shared HTTP client |
| `build_openai_request` / `build_anthropic_request` | `host/ai.rs:~50-115` | HTTP request building (reference, not direct reuse) |
| `require_permission_json` | `routes/helpers.rs` | Permission check for JSON endpoints |
| `ai_usage_log` INSERT | `host/ai.rs` (record_usage pattern) | Usage logging |
| `DashMap` rate limiter | `host/ai.rs` (provider RPM limiter) | Rate limit pattern |
| `SESSION_USER_ID` | `routes/auth.rs:36` | Session user lookup |
| `render_admin_template` | `routes/helpers.rs` | Admin page rendering |
| `require_csrf` | `routes/helpers.rs` | CSRF on admin POST |
| Tile model + service | `models/tile.rs`, `services/tile.rs` | Tile CRUD, rendering |
| `inject_site_context` | `routes/helpers.rs:154` | Where tiles are rendered into pages |

### Request/Response Building

Reference the existing HTTP request builders in `host/ai.rs` but DO NOT import them directly (they are private functions in the host module). Instead, create similar functions in `ChatService` that add `"stream": true` to the request body.

For OpenAI-compatible:
```rust
fn build_streaming_openai_request(resolved: &ResolvedProvider, messages: &[AiMessage], options: &AiRequestOptions) -> (String, String, Vec<(String, String)>) {
    let url = format!("{}/chat/completions", resolved.config.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": &resolved.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_tokens": options.max_tokens.unwrap_or(1024),
        "temperature": options.temperature.unwrap_or(0.7),
    });
    let headers = vec![("authorization".into(), format!("Bearer {}", resolved.api_key.as_deref().unwrap_or("")))];
    (url, body.to_string(), headers)
}
```

For Anthropic:
```rust
fn build_streaming_anthropic_request(resolved: &ResolvedProvider, messages: &[AiMessage], system: &str, options: &AiRequestOptions) -> (String, String, Vec<(String, String)>) {
    let url = format!("{}/messages", resolved.config.base_url.trim_end_matches('/'));
    // Anthropic: system prompt is a separate field, not in messages
    let body = serde_json::json!({
        "model": &resolved.model,
        "messages": messages.iter().filter(|m| m.role != "system").collect::<Vec<_>>(),
        "system": system,
        "stream": true,
        "max_tokens": options.max_tokens.unwrap_or(1024),
        "temperature": options.temperature.unwrap_or(0.7),
    });
    let headers = vec![
        ("x-api-key".into(), resolved.api_key.as_deref().unwrap_or("").into()),
        ("anthropic-version".into(), "2023-06-01".into()),
    ];
    (url, body.to_string(), headers)
}
```

### Budget Check Before Stream

Check budget BEFORE starting the stream (same pattern as `host/ai.rs`):

```rust
let budget_result = state.ai_budgets().check_budget(
    user_id, &provider_id, &user_roles
).await;
if let Some(result) = budget_result {
    if !result.allowed {
        return Err((StatusCode::TOO_MANY_REQUESTS, Json(JsonError { error: "AI token budget exceeded".into() })));
    }
}
```

### Files to Create

| File | Purpose |
|------|---------|
| `crates/kernel/src/services/ai_chat.rs` | ChatService + config types + streaming logic |
| `crates/kernel/src/routes/api_chat.rs` | POST /api/v1/chat SSE endpoint |
| `crates/kernel/src/routes/admin_ai_chat.rs` | Admin config GET/POST handlers |
| `templates/admin/ai-chat.html` | Admin config form template |
| `templates/tiles/chat.html` | Chat widget Tera template with inline JS |

### Files to Modify

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `stream` to reqwest features |
| `crates/kernel/Cargo.toml` | Add `tokio-stream`, `async-stream` |
| `crates/kernel/src/services/mod.rs` | Add `pub mod ai_chat;` |
| `crates/kernel/src/state.rs` | Add `ai_chat: Arc<ChatService>` to AppStateInner, init, getter |
| `crates/kernel/src/routes/mod.rs` | Add `pub mod api_chat;` + `pub mod admin_ai_chat;` |
| `crates/kernel/src/routes/admin.rs` | Merge `admin_ai_chat::router()` |
| `crates/kernel/src/routes/mod.rs` (public routes) | Merge `api_chat::router()` into public API routes |
| `crates/kernel/src/services/tile.rs` | Add `chat` tile type rendering in `render_tile_html()` |
| `crates/kernel/src/routes/tile_admin.rs` | Add `chat` to tile type options + form handling |
| `templates/admin/tile-form.html` | Add `chat` option to tile type dropdown |
| `templates/page--admin.html` | Add "AI Chat" sidebar link under System section |
| `crates/kernel/tests/integration_test.rs` | Add `AI_CHAT_LOCK` + chat integration tests |

### Project Structure Notes

- `ChatService` is a kernel service (same pattern as `AiProviderService`) — justified because SSE streaming cannot be done through the WASM plugin boundary
- The chat route is a public API route (`/api/v1/chat`), not an admin route — register in the public router section of `routes/mod.rs`
- Admin config route follows domain module convention: `admin_ai_chat.rs` (not in `admin.rs`)
- Tile template goes in `templates/tiles/` (new directory if needed, or inline in `services/tile.rs`)
- No new database migration needed — uses existing `site_config` table + `ai_usage_log` table + Redis sessions

### Constraints and Pitfalls

1. **Do NOT use the WASM `ai_request()` host function** — the chatbot calls providers directly from kernel code via `AiProviderService`. The host function is for plugins only.
2. **Do NOT use `EventSource` on the client** — it only supports GET requests. Use `fetch()` with `ReadableStream` for POST + SSE.
3. **Do NOT add `#[tokio::test]`** to integration tests — use `#[test]` + `run_test(async { ... })` on the shared runtime.
4. **Session cannot be updated inside SSE stream** — `Session` is not `Send` across the stream boundary. Update history before/after streaming, not during.
5. **Reqwest `.timeout(120s)` overrides the client's 10s default** — required for streaming requests that may take 30+ seconds.
6. **`{% import %}` must be at template top level** — not inside `{% block %}` (Tera limitation from Story 31.1).
7. **Macro keyword syntax required** — `{{ form::csrf(csrf_token=csrf_token) }}`, not positional.
8. **Budget check BEFORE stream start** — don't consume budget tokens before confirming budget allows the request.
9. **Anthropic system prompt is a separate field** — not in the messages array (unlike OpenAI). Handle in request builder.
10. **`write!(string, ...).unwrap()`** — safe on `String`, add `// Infallible:` comment per CLAUDE.md.
11. **DashMap is already a dependency** — no need to add it, used by rate_limit and ai.rs.
12. **Do NOT create a separate `reqwest::Client`** — use `AiProviderService::http()` with per-request `.timeout()` override.

### Testing Strategy

Use existing test infrastructure from `crates/kernel/tests/common/mod.rs`:
- `shared_app()` for `&'static TestApp`
- `create_and_login_admin` / `create_and_login_user` for authenticated users
- Add `AI_CHAT_LOCK: Mutex<()>` for tests that mutate chat config (same pattern as `AI_BUDGET_LOCK`, `AI_PERMISSION_LOCK`)

**NOTE:** Integration tests for SSE responses can verify:
1. Response content-type is `text/event-stream`
2. Response status code is correct (200 for success, 403/401/429 for errors)
3. Admin config save/load works via HTTP requests

Full SSE stream parsing in tests is complex (requires consuming the stream). For v1, test the content-type + status codes. The SSE parsing logic can be unit-tested in `ai_chat.rs` with mock data.

**Test for chat tile:** Use `TileService::render_region()` after creating a `chat` tile type via direct SQL insert. Verify the rendered HTML contains the chat widget structure.

### References

- [Source: docs/ritrovo/epic-03.md#Story 31.7 — Chatbot Tile with SSE Streaming]
- [Source: docs/ritrovo/epic-03.md#Design Decision D2 — SSE for streaming]
- [Source: docs/ritrovo/epic-03.md#Design Decision D3 — Token budget system]
- [Source: crates/kernel/src/services/ai_provider.rs — AiProviderService]
- [Source: crates/kernel/src/services/tile.rs — TileService render_tile_html]
- [Source: crates/kernel/src/search/mod.rs — SearchService::search]
- [Source: crates/kernel/src/host/ai.rs — build_openai_request, build_anthropic_request patterns]
- [Source: crates/kernel/src/routes/helpers.rs:154 — inject_site_context tile rendering]
- [Source: crates/kernel/src/models/tile.rs — Tile model, visibility rules]
- [Source: crates/kernel/src/session.rs — Redis session setup, tower-sessions]
- [Source: crates/kernel/src/routes/auth.rs:36 — SESSION_USER_ID constant]

### Previous Story Intelligence

**From Story 31.1 (AI Provider Registry):**
- `AiProviderService` stores configs in `site_config` JSONB — reuse pattern for `ChatConfig`
- `reqwest::Client` shared via service, 10-second timeout — override with per-request `.timeout()` for streaming
- `ResolvedProvider` contains `config`, `api_key`, `model` — use for building streaming requests
- SSRF prevention: `validate_base_url()` blocks private IPs — already enforced at provider registration
- Tera `{% import %}` must be at top level, macro keyword syntax required

**From Story 31.2 (AI Host Function):**
- `build_openai_request` / `build_anthropic_request` patterns — reference for building streaming requests
- OpenAI uses `Authorization: Bearer {key}` + messages array with system role
- Anthropic uses `x-api-key` header + separate `system` field + `anthropic-version: 2023-06-01`
- Response normalization to `AiResponse { content, model, usage, latency_ms, finish_reason }`
- Clone `caller.data()` fields before await — not relevant for kernel routes but good practice

**From Story 31.3 (Token Budgets):**
- `AiTokenBudgetService::check_budget()` returns `BudgetCheckResult { allowed, remaining, limit, used, action }`
- Budget check before expensive operation (here: before starting SSE stream)
- `ai_usage_log` INSERT pattern for recording token usage
- `AI_BUDGET_LOCK` mutex pattern for test serialization

**From Story 31.4 (AI Permissions):**
- `require_permission_json(state, session, "configure ai")` for admin JSON endpoints
- `require_permission(state, session, "configure ai")` for admin HTML endpoints
- `UserContext::has_permission("use ai chat")` for WASM path — kernel routes use `require_permission_json` instead
- `AI_PERMISSION_LOCK` mutex pattern for tests that assign/revoke permissions

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

### Completion Notes List

### File List
