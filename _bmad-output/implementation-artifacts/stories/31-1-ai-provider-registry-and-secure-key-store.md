# Story 31.1: AI Provider Registry & Secure Key Store

Status: done

## Story

As a **site administrator**,
I want to register AI providers (OpenAI, Anthropic, local models) with their connection details,
so that plugins and kernel services can make AI requests without managing credentials themselves.

## Acceptance Criteria

1. **AC1: Provider CRUD** — Admin UI at `/admin/system/ai-providers` lists all configured providers. Add/edit form collects: label, protocol (OpenAI-compatible or Anthropic), base URL, API key env var name, rate limit RPM, enabled toggle, and operation-to-model mappings.

2. **AC2: Secure Key Storage** — API keys are stored as environment variable names only (e.g., `OPENAI_API_KEY`), never as raw key values. The `AiProviderService` resolves the env var at runtime via `std::env::var()`. Keys never appear in database rows, templates, or WASM memory.

3. **AC3: Env Var Validation** — Env var names are validated against an allowlist pattern and blocked from known-sensitive process variables (PATH, HOME, etc.) to prevent information disclosure.

4. **AC4: SSRF Prevention** — Base URLs are validated for scheme (http/https only) and blocked from targeting private/link-local network ranges to prevent SSRF attacks.

5. **AC5: Operation Defaults** — Admin can assign a default provider per operation type (Chat, Embedding, Image Generation, Speech-to-Text, Text-to-Speech, Moderation) at `/admin/system/ai-providers/defaults`. Stored in `site_config` as `"ai_defaults"`.

6. **AC6: Provider Resolution** — `AiProviderService::resolve_provider(operation, optional_provider_id)` returns a `ResolvedProvider` with config, resolved API key, and model string. Falls back to the default provider for the operation type when no override is given.

7. **AC7: Connection Test** — "Test Connection" button on the provider form sends a minimal request to the configured endpoint and reports success/failure with latency.

8. **AC8: Config Serialization** — Provider configs stored as JSONB in `site_config` table under keys `"ai_providers"` and `"ai_defaults"`. CRUD serialized via `tokio::sync::Mutex` to prevent lost updates.

9. **AC9: Protocol Support** — Two wire protocols supported: `OpenAiCompatible` (covers OpenAI, Azure, Ollama, vLLM) and `Anthropic` (Messages API). Each protocol has distinct HTTP headers and request body format.

10. **AC10: Integration Tests** — Tests verify: (a) provider CRUD via admin routes; (b) connection test endpoint; (c) defaults save/load; (d) SSRF-blocked URLs rejected; (e) env var denylist enforced.

## Dev Notes

### Key Implementation Details

- `AiProviderService` (848 lines) in `crates/kernel/src/services/ai_provider.rs`
- Admin routes (699 lines) in `crates/kernel/src/routes/admin_ai_provider.rs`
- Provider configs: `AiProviderConfig` with `id`, `label`, `protocol`, `base_url`, `api_key_env`, `models: Vec<OperationModel>`, `rate_limit_rpm`, `enabled`
- `ResolvedProvider` is intentionally NOT `Serialize` to prevent key leakage
- Operation types: `Chat`, `Embedding`, `ImageGeneration`, `SpeechToText`, `TextToSpeech`, `Moderation`
- Protocol types: `OpenAiCompatible`, `Anthropic`
- Shared `reqwest::Client` with 10-second default timeout
- Admin template: `templates/admin/ai-providers.html`, `templates/admin/ai-provider-form.html`

### Files

**Created:**
- `crates/kernel/src/services/ai_provider.rs` — AiProviderService, config types, validation, resolution
- `crates/kernel/src/routes/admin_ai_provider.rs` — Admin CRUD routes for providers and defaults
- `templates/admin/ai-providers.html` — Provider list template
- `templates/admin/ai-provider-form.html` — Provider add/edit form template

**Modified:**
- `crates/kernel/src/services/mod.rs` — Added `pub mod ai_provider;`
- `crates/kernel/src/state.rs` — Added `ai_providers: Arc<AiProviderService>` to AppStateInner
- `crates/kernel/src/routes/mod.rs` — Added `pub mod admin_ai_provider;`, merged router
- `crates/kernel/src/routes/admin.rs` — Merged admin_ai_provider router
- `templates/page--admin.html` — Added "AI Providers" sidebar link under System section

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Provider config stored in `site_config` JSONB (not a dedicated table) — simple, no migration needed
- Env var resolution at runtime ensures keys rotate without DB changes
- SSRF prevention validates base URLs block private ranges (10.x, 172.16-31.x, 192.168.x, 169.254.x, localhost, [::1])
- `tokio::sync::Mutex` serializes config writes to prevent lost updates from concurrent read-modify-write
- Connection test sends minimal request to verify API key and endpoint reachability

### File List

- `crates/kernel/src/services/ai_provider.rs` (848 lines)
- `crates/kernel/src/routes/admin_ai_provider.rs` (699 lines)
- `templates/admin/ai-providers.html`
- `templates/admin/ai-provider-form.html`
