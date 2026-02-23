# Story 31.2: `ai_request()` Host Function

Status: done

## Story

As a **plugin developer**,
I want to **call a single host function to make AI requests**,
so that **I can use any AI provider without managing connections or credentials**.

## Acceptance Criteria

1. `ai_request()` host function registered in `crates/kernel/src/host/` module under the `trovato:kernel/ai-api` WIT interface
2. Accepts JSON-serialized `AiRequest` with operation type, optional model override, messages/input, and options (`max_tokens`, `temperature`, etc.)
3. Resolves provider from site config (via `AiProviderService::resolve_provider()`) based on operation type
4. Injects API key from secure key store (env var) before making the HTTP request — key never crosses the WASM boundary
5. Makes HTTP request to provider API using the correct protocol (OpenAI-compatible or Anthropic)
6. Returns normalized JSON `AiResponse` with `content`, `usage` (prompt + completion tokens), `model`, and `latency_ms`
7. Enforces rate limits from provider config (`rate_limit_rpm`)
8. Logs every request with tracing: token count, latency, model, calling plugin name, operation type
9. Handles provider errors gracefully: timeout, rate limit (429), auth failure (401/403), malformed response
10. SDK types added to `crates/plugin-sdk/src/types.rs`: `AiRequest`, `AiResponse`, `AiMessage`, `AiRequestOptions`
11. SDK host binding added to `crates/plugin-sdk/src/host.rs`: `ai_request(request_json: &str) -> Result<String, i32>`
12. WIT interface `ai-api` added to `crates/wit/kernel.wit` with `ai-request` function
13. New error codes added to `crates/plugin-sdk/src/host_errors.rs` for AI-specific failures
14. `RequestServices` extended to include `AiProviderService` reference so host functions can access it

## Tasks / Subtasks

- [ ] Task 1: Add SDK types for AI request/response (AC: #10)
  - [ ] 1.1 Add `AiRequest`, `AiResponse`, `AiMessage`, `AiRequestOptions` structs to `crates/plugin-sdk/src/types.rs`
  - [ ] 1.2 Add AI error codes to `crates/plugin-sdk/src/host_errors.rs`: `ERR_AI_NO_PROVIDER`, `ERR_AI_REQUEST_FAILED`, `ERR_AI_RATE_LIMITED`, `ERR_AI_INVALID_REQUEST`
- [ ] Task 2: Add WIT interface (AC: #12)
  - [ ] 2.1 Add `ai-api` interface to `crates/wit/kernel.wit` with `ai-request: func(request-json: string) -> result<string, string>;`
  - [ ] 2.2 Add `import ai-api;` to the `plugin` world
- [ ] Task 3: Extend `RequestServices` with AI provider access (AC: #14)
  - [ ] 3.1 Add `ai_providers: Arc<AiProviderService>` field to `RequestServices` in `crates/kernel/src/tap/request_state.rs`
  - [ ] 3.2 Add `reqwest::Client` to `RequestServices` (shared HTTP client for AI requests)
  - [ ] 3.3 Update all `RequestServices` construction sites (in `state.rs`, `cron/mod.rs`, dispatcher, etc.)
- [ ] Task 4: Implement the `ai_request()` host function (AC: #1, #2, #3, #4, #5, #6, #7, #8, #9)
  - [ ] 4.1 Create `crates/kernel/src/host/ai.rs` with `register_ai_functions()`
  - [ ] 4.2 Implement async host function using `func_wrap_async` pattern from `db.rs`
  - [ ] 4.3 Deserialize `AiRequest` JSON from WASM memory
  - [ ] 4.4 Call `AiProviderService::resolve_provider()` to get `ResolvedProvider`
  - [ ] 4.5 Build HTTP request based on `ProviderProtocol` (OpenAI-compatible vs Anthropic)
  - [ ] 4.6 Execute HTTP request with the resolved API key injected in the auth header
  - [ ] 4.7 Parse provider response into normalized `AiResponse`
  - [ ] 4.8 Log request via tracing (plugin name, operation, model, tokens, latency)
  - [ ] 4.9 Serialize `AiResponse` as JSON and write to WASM output buffer
  - [ ] 4.10 Handle errors: no provider configured, API key missing, HTTP errors, rate limits
- [ ] Task 5: Register the host function (AC: #1)
  - [ ] 5.1 Add `mod ai;` and `pub use ai::register_ai_functions;` to `crates/kernel/src/host/mod.rs`
  - [ ] 5.2 Call `register_ai_functions(linker)?;` in `register_all()`
- [ ] Task 6: Add SDK host binding (AC: #11)
  - [ ] 6.1 Add `#[link(wasm_import_module = "trovato:kernel/ai-api")]` extern block to `crates/plugin-sdk/src/host.rs`
  - [ ] 6.2 Add ergonomic `ai_request(request: &AiRequest) -> Result<AiResponse, i32>` wrapper
  - [ ] 6.3 Add native-target test stub
- [ ] Task 7: Unit tests (AC: all)
  - [ ] 7.1 Test `AiRequest`/`AiResponse` serde roundtrips in plugin-sdk
  - [ ] 7.2 Test request building for OpenAI-compatible protocol
  - [ ] 7.3 Test request building for Anthropic protocol
  - [ ] 7.4 Test error code constants are unique and non-overlapping
  - [ ] 7.5 Test response parsing from mock provider JSON

## Dev Notes

### Architecture

This story implements **Layer 1.3** from the [AI Integration design](docs/design/ai-integration.md): the `ai_request()` host function. This is the single integration point for all AI operations from plugins. The kernel handles provider resolution, API key injection, HTTP execution, and response normalization. Plugins never see API keys or make direct HTTP calls to AI providers.

**Critical security invariant:** The `ResolvedProvider` struct (which contains the API key) is intentionally NOT `Serialize` — it must NEVER cross the WASM boundary. The host function resolves the provider, makes the HTTP call, and returns only the normalized response.

### Async Host Function Pattern

The `ai_request()` host function MUST use `func_wrap_async` (not `func_wrap`) because it performs async HTTP requests. Follow the exact pattern from `crates/kernel/src/host/db.rs:507`:

```rust
linker.func_wrap_async(
    "trovato:kernel/ai-api",
    "ai-request",
    |mut caller: wasmtime::Caller<'_, PluginState>,
     (req_ptr, req_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
        Box::new(async move {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return host_errors::ERR_MEMORY_MISSING;
            };
            // ... read request JSON, resolve provider, make HTTP call, write response
        })
    },
)?;
```

### Service Access Pattern

Services are accessed from the WASM store via `caller.data().request.services()`. The `RequestServices` struct (in `crates/kernel/src/tap/request_state.rs:65`) currently holds `db: PgPool` and `lockout: Option<Arc<LockoutService>>`. This story adds `ai_providers: Arc<AiProviderService>` and a shared `reqwest::Client` for making outbound HTTP requests.

The `for_background()` constructor also needs updating to include the AI provider service.

### HTTP Request Building

Two protocols must be supported (matching `ProviderProtocol` from Story 31.1):

**OpenAI-compatible** (`POST {base_url}/chat/completions`):
```json
{
  "model": "gpt-4o",
  "messages": [{"role": "system", "content": "..."}, {"role": "user", "content": "..."}],
  "max_tokens": 200,
  "temperature": 0.3
}
// Auth: Authorization: Bearer {api_key}
```

**Anthropic** (`POST {base_url}/messages`):
```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 200,
  "messages": [{"role": "user", "content": "..."}],
  "system": "You are a helpful assistant."
}
// Auth: x-api-key: {api_key}, anthropic-version: 2023-06-01
```

The system message handling differs: OpenAI puts system messages in the messages array; Anthropic uses a separate `system` field. The host function must handle this translation.

### Response Normalization

Both protocols return different response shapes. Normalize to:

```rust
pub struct AiResponse {
    pub content: String,          // The generated text
    pub model: String,            // Model that was actually used
    pub usage: AiUsage,           // Token counts
    pub latency_ms: u64,          // Round-trip time
    pub finish_reason: Option<String>, // "stop", "length", etc.
}

pub struct AiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
```

### Error Handling

New error codes (add to `host_errors.rs` starting at -20 to leave room):

| Code | Constant | Meaning |
|------|----------|---------|
| `-20` | `ERR_AI_NO_PROVIDER` | No provider configured for the requested operation type |
| `-21` | `ERR_AI_REQUEST_FAILED` | HTTP request to provider failed (timeout, network error) |
| `-22` | `ERR_AI_RATE_LIMITED` | Provider rate limit exceeded (429) or local RPM limit |
| `-23` | `ERR_AI_INVALID_REQUEST` | Malformed AiRequest JSON from plugin |
| `-24` | `ERR_AI_AUTH_FAILED` | Provider returned 401/403 (bad API key) |
| `-25` | `ERR_AI_PROVIDER_ERROR` | Provider returned non-2xx error (500, etc.) |

For non-fatal errors that still return useful information (e.g., provider returned an error message), write the error details as JSON to the output buffer and return the negative error code. This allows plugins to inspect the error.

### WIT Interface Addition

Add to `crates/wit/kernel.wit` (after the `logging` interface, before the `world plugin` block):

```wit
/// AI API — access to AI providers via the kernel's secure provider registry.
/// Plugins submit requests as JSON; the kernel resolves the provider,
/// injects the API key, and returns a normalized response.
interface ai-api {
    ai-request: func(request-json: string) -> result<string, string>;
}
```

Add `import ai-api;` to the `world plugin` block.

### SDK Types Location

Add to `crates/plugin-sdk/src/types.rs` (these types are shared between kernel and plugins):

- `AiRequest` — operation type, optional provider/model override, messages, options
- `AiResponse` — content, model, usage, latency
- `AiMessage` — role + content
- `AiRequestOptions` — max_tokens, temperature, top_p, stop sequences
- `AiUsage` — prompt_tokens, completion_tokens, total_tokens
- Re-export `AiOperationType` from kernel? No — define a duplicate in plugin-sdk since plugin-sdk cannot depend on kernel. Use the same serde representation (`snake_case`) so JSON is compatible.

### Rate Limiting

Simple per-provider RPM tracking in the host function. Use an in-memory counter (e.g., `DashMap<String, (u64, Instant)>` mapping provider_id to (count, window_start)). When count exceeds `rate_limit_rpm` within the current 60-second window, return `ERR_AI_RATE_LIMITED`. This is a best-effort local rate limiter, not distributed — sufficient for single-instance deployments. The provider's own rate limiting (HTTP 429) is handled separately as `ERR_AI_RATE_LIMITED`.

### Observability

Every `ai_request()` call is logged at `info` level with structured fields:

```rust
tracing::info!(
    plugin = %plugin_name,
    operation = %operation_type,
    model = %model,
    prompt_tokens = usage.prompt_tokens,
    completion_tokens = usage.completion_tokens,
    latency_ms = latency,
    "ai_request completed"
);
```

This feeds future Story 31.9 (admin usage dashboard) and Story 31.3 (token budgets).

### Project Structure Notes

- Host function file: `crates/kernel/src/host/ai.rs` — follows established pattern (`host/db.rs`, `host/item.rs`, etc.)
- WIT module name: `trovato:kernel/ai-api` — follows existing naming (`trovato:kernel/db`, `trovato:kernel/item-api`)
- SDK import module: `#[link(wasm_import_module = "trovato:kernel/ai-api")]` — matches WIT
- Error codes: `-20` through `-25` — avoids collision with existing `-1` through `-15`

### What NOT to Do

- Do NOT expose the API key to the WASM module in any form
- Do NOT add `Serialize` to `ResolvedProvider`
- Do NOT use `func_wrap` (sync) — HTTP requests are async, must use `func_wrap_async`
- Do NOT create a new `reqwest::Client` per request — use the shared client from `RequestServices`
- Do NOT implement streaming (SSE) in this story — that's Story 31.7 (chatbot)
- Do NOT implement token budget enforcement — that's Story 31.3
- Do NOT implement permission checks — that's Story 31.4
- Do NOT duplicate `AiOperationType` as a trait or complex enum in plugin-sdk — keep it as a simple serde-compatible enum that matches the kernel's JSON format

### References

- [Source: docs/design/ai-integration.md#Layer 1: AI Core (Kernel-Level)] — Three-layer architecture, `ai_request()` spec
- [Source: docs/ritrovo/epic-03.md#Story 31.2] — Epic acceptance criteria
- [Source: crates/kernel/src/host/db.rs:501-537] — Canonical `func_wrap_async` pattern
- [Source: crates/kernel/src/host/mod.rs] — Host function registration and memory helpers
- [Source: crates/kernel/src/tap/request_state.rs:64-78] — `RequestServices` struct to extend
- [Source: crates/kernel/src/services/ai_provider.rs:447-480] — `resolve_provider()` method
- [Source: crates/kernel/src/services/ai_provider.rs:136-144] — `ResolvedProvider` (NOT Serialize)
- [Source: crates/plugin-sdk/src/host_errors.rs] — Error code conventions
- [Source: crates/plugin-sdk/src/host.rs] — SDK FFI binding pattern with `#[link(wasm_import_module)]`
- [Source: crates/wit/kernel.wit] — WIT interface definitions and `world plugin`
- [Source: crates/kernel/src/state.rs:108] — `ai_providers` already wired into AppState (from Story 31.1)

## Previous Story Intelligence

### Story 31.1 Learnings

Story 31.1 (AI Provider Registry) was completed in commit `5394709`. Key patterns established:

1. **Tera template imports must be at top level** — `{% import %}` outside `{% block %}`, never inside. This caused 61/114 test failures when templates broke Tera's global template loading.
2. **Tera macro arguments must use keyword syntax** — `{{ form::csrf(csrf_token=csrf_token) }}` not `{{ form::csrf(csrf_token) }}`.
3. **The `AiProviderService` is always-initialized** (like `SearchService`) — not plugin-gated, since multiple future plugins depend on it.
4. **Provider configs stored as JSONB** in existing `site_config` table with keys `"ai_providers"` and `"ai_defaults"`.
5. **`reqwest` is already a kernel dependency** — used for connection testing. The HTTP client from `AiProviderService` has a 10-second timeout.
6. **SSRF prevention** — `validate_base_url()` blocks private IPs, localhost, cloud metadata endpoints. The `ai_request()` host function should reuse this validation (or rely on the fact that providers are admin-configured and already validated at save time).
7. **Env var denylist** — `DENIED_ENV_VARS` prevents exfiltration of DB credentials, session secrets, etc.

### Files Created/Modified in 31.1

- `crates/kernel/src/services/ai_provider.rs` (843 lines) — Service + types + validation + tests
- `crates/kernel/src/routes/admin_ai_provider.rs` (699 lines) — Admin UI handlers
- `crates/kernel/src/state.rs` — Added `ai_providers: Arc<AiProviderService>` to AppState
- `crates/kernel/src/services/mod.rs` — Added `pub mod ai_provider;`
- `crates/kernel/src/routes/mod.rs` — Added `pub mod admin_ai_provider;`
- `crates/kernel/src/routes/admin.rs` — Merged AI provider router
- `templates/admin/ai-providers.html` — Provider list + defaults form
- `templates/admin/ai-provider-form.html` — Add/edit form
- `templates/page--admin.html` — Sidebar link

## Git Intelligence

Recent commits show:
- `5394709` feat: add AI provider registry with admin UI (Story 31.1) — Most recent, directly prerequisite
- `1f19a27` feat: add tested tutorial assertions for Part 1 (19 tests)
- `b86bc5b` feat: add conference gather at /conferences and align tutorial with epic

Conventions: imperative commit messages, `feat:` / `fix:` / `docs:` prefixes, story reference in message.

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6

### Completion Notes List
- Task 1: SDK types (AiRequest, AiResponse, AiMessage, AiRequestOptions, AiUsage, AiOperationType) added to plugin-sdk/types.rs with 5 serde roundtrip tests. AI error codes (-20 to -25) added to host_errors.rs.
- Task 2: WIT interface `ai-api` added to kernel.wit with `ai-request` function. `import ai-api` added to `world plugin`.
- Task 3: `RequestServices` extended with `ai_providers: Option<Arc<AiProviderService>>`. `for_background()` updated. Public `http()` getter added to `AiProviderService`.
- Task 4: `crates/kernel/src/host/ai.rs` created (470 lines). Implements: request deserialization, provider resolution, rate limiting (in-memory per-provider RPM), HTTP request building (OpenAI-compatible + Anthropic protocols), response parsing/normalization, structured tracing, error handling with all 6 AI error codes. 9 unit tests.
- Task 5: Host function registered in `host/mod.rs` via `register_ai_functions()` in `register_all()`.
- Task 6: SDK binding added to `host.rs` with `#[link(wasm_import_module = "trovato:kernel/ai-api")]` extern block, ergonomic `ai_request()` wrapper, and native test stub. 1 test.
- Design decision: `reqwest::Client` not added as separate field on `RequestServices` — `AiProviderService` already has an HTTP client (10s timeout) exposed via new `http()` getter.
- All 629 kernel lib tests pass (9 new), 11 SDK tests pass (2 new). Zero clippy warnings. Formatting clean.

### File List
- `crates/kernel/src/host/ai.rs` (CREATED) — AI host function implementation
- `crates/kernel/src/host/mod.rs` (MODIFIED) — Added mod ai, pub use, register_ai_functions call
- `crates/kernel/src/tap/request_state.rs` (MODIFIED) — Added ai_providers field to RequestServices
- `crates/kernel/src/services/ai_provider.rs` (MODIFIED) — Added http() getter
- `crates/plugin-sdk/src/types.rs` (MODIFIED) — Added AI types
- `crates/plugin-sdk/src/host_errors.rs` (MODIFIED) — Added AI error codes
- `crates/plugin-sdk/src/host.rs` (MODIFIED) — Added AI host binding + stub
- `crates/wit/kernel.wit` (MODIFIED) — Added ai-api interface
