# Story 31.2: ai_request() Host Function

Status: done

## Story

As a **plugin developer**,
I want to call a single host function to make AI requests,
so that I can use any AI provider without managing connections or credentials.

## Acceptance Criteria

1. **AC1: Host Function Registration** — `ai_request()` host function registered in `crates/kernel/src/host/ai.rs` under the `trovato:kernel/ai-api` WIT interface.

2. **AC2: Request Format** — Accepts JSON-serialized `AiRequest` with operation type, optional provider/model overrides, messages (for chat) or input text (for embedding/moderation), and options (`max_tokens`, `temperature`, `top_p`, `stop`).

3. **AC3: Provider Resolution** — Resolves provider from site config via `AiProviderService::resolve_provider()` based on operation type. Supports optional provider ID and model overrides from the plugin.

4. **AC4: Secure Key Injection** — Injects API key from secure key store (env var) before making the HTTP request. Key never crosses the WASM boundary.

5. **AC5: Protocol Handling** — Makes HTTP request using the correct protocol: OpenAI-compatible (Bearer token, messages array with system role) or Anthropic (x-api-key header, separate system field, anthropic-version header).

6. **AC6: Normalized Response** — Returns JSON `AiResponse` with `content`, `model`, `usage` (prompt_tokens, completion_tokens, total_tokens), `latency_ms`, and optional `finish_reason`.

7. **AC7: Rate Limiting** — In-memory per-provider RPM rate limiter using fixed 60-second sliding windows. Returns `ERR_AI_RATE_LIMITED` (-22) when exceeded. Stale entries evicted automatically.

8. **AC8: Request Logging** — Every request logged via tracing: token counts, latency, model, calling plugin name, operation type, provider ID.

9. **AC9: Error Handling** — Graceful handling of: timeout, rate limit (HTTP 429), auth failure (401/403), malformed request, provider error (5xx). Each maps to a distinct error code (-20 through -27).

10. **AC10: SDK Types** — `AiRequest`, `AiResponse`, `AiMessage`, `AiRequestOptions`, `AiUsage`, `AiOperationType` added to `crates/plugin-sdk/src/types.rs` with serde derive.

11. **AC11: SDK Host Binding** — `ai_request(request_json: &str) -> Result<String, i32>` binding in `crates/plugin-sdk/src/host.rs`.

12. **AC12: WIT Interface** — `ai-api` interface added to `crates/wit/kernel.wit` with `ai-request` function signature.

13. **AC13: Error Codes** — AI-specific error codes (-20 through -27) added to `crates/plugin-sdk/src/host_errors.rs`: `ERR_AI_NO_PROVIDER`, `ERR_AI_REQUEST_FAILED`, `ERR_AI_RATE_LIMITED`, `ERR_AI_INVALID_REQUEST`, `ERR_AI_AUTH_FAILED`, `ERR_AI_PROVIDER_ERROR`, `ERR_AI_BUDGET_EXCEEDED`, `ERR_AI_PERMISSION_DENIED`.

14. **AC14: RequestServices Extension** — `RequestServices` extended to include `AiProviderService` reference so host functions can access provider resolution and HTTP client.

## Dev Notes

### Key Implementation Details

- `host/ai.rs` (959 lines) implements the full request lifecycle: deserialize, validate, resolve provider, check rate limit, build protocol-specific HTTP request, execute, parse response, normalize, return
- Rate limiter uses `LazyLock<Mutex<HashMap<String, RateWindow>>>` with `AtomicU64` counters per provider
- OpenAI request: `POST {base_url}/chat/completions` with `Authorization: Bearer {key}` header
- Anthropic request: `POST {base_url}/messages` with `x-api-key` header + `anthropic-version: 2023-06-01`
- Response normalization extracts content, model, usage, and finish_reason from protocol-specific JSON shapes
- `func_wrap_async` with `Box::new(async move { ... })` pattern for async WASM host functions
- All `caller.data()` fields cloned before any `.await` calls (borrow cannot be held across await points)

### Files

**Created:**
- `crates/kernel/src/host/ai.rs` — ai_request host function implementation (959 lines)

**Modified:**
- `crates/kernel/src/host/mod.rs` — Added `pub mod ai;`, registered ai_request in linker
- `crates/plugin-sdk/src/types.rs` — Added AiRequest, AiResponse, AiMessage, AiRequestOptions, AiUsage, AiOperationType, AiRequestContext, AiRequestDecision
- `crates/plugin-sdk/src/host.rs` — Added ai_request SDK binding
- `crates/plugin-sdk/src/host_errors.rs` — Added ERR_AI_NO_PROVIDER through ERR_AI_PERMISSION_DENIED (-20 to -27)
- `crates/wit/kernel.wit` — Added ai-api interface with ai-request function
- `crates/kernel/src/tap/request_state.rs` — Extended RequestServices with AiProviderService

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Protocol-specific request builders handle the significant differences between OpenAI and Anthropic APIs (system prompt placement, header format, response shape)
- Rate limiter evicts stale entries when map exceeds 50 entries to prevent unbounded growth
- Error codes follow a clean range: -20 (no provider) through -27 (permission denied)
- SDK types include `AiRequestContext` and `AiRequestDecision` for future `tap_ai_request` governance hook

### File List

- `crates/kernel/src/host/ai.rs` (959 lines)
- `crates/plugin-sdk/src/types.rs` (AiRequest, AiResponse, AiMessage, AiRequestOptions, AiUsage, AiOperationType)
- `crates/plugin-sdk/src/host_errors.rs` (ERR_AI_NO_PROVIDER through ERR_AI_PERMISSION_DENIED)
- `crates/plugin-sdk/src/host.rs` (ai_request binding)
- `crates/wit/kernel.wit` (ai-api interface)
