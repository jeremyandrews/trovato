# Story 47.6: Per-AI-Feature Configuration

Status: ready-for-dev

## Story

As a site operator,
I want granular control over which AI operation types are enabled,
so that I can enable chat but disable image generation, or use different providers for different operations.

## Acceptance Criteria

1. Site config gains `ai_features` section with per-operation `enabled`, `provider`, and `model` settings
2. `ai_request()` checks feature config before dispatching — disabled operations return error to calling plugin
3. Per-feature provider override supported (e.g., "use Anthropic for chat, OpenAI for embeddings")
4. Per-feature model override supported (e.g., "use claude-sonnet for chat, claude-haiku for moderation")
5. Default: all operations enabled with global provider/model config (backward compatible)
6. Admin UI at `/admin/config/ai` with per-operation toggles, provider, and model selection
7. At least 2 integration tests: disabled operation returns error, per-operation provider selection works

## Tasks / Subtasks

- [ ] Define `ai_features` config schema with per-operation settings (AC: #1)
  - [ ] Operations: Chat, Embedding, ImageGeneration, SpeechToText, TextToSpeech, Moderation
  - [ ] Each has: enabled (bool), provider (optional string), model (optional string)
- [ ] Add feature check to `ai_request()` dispatch path (AC: #2)
  - [ ] Return descriptive error when disabled: "AI operation {type} is disabled in site configuration"
- [ ] Implement per-feature provider/model resolution (AC: #3, #4)
  - [ ] Resolution chain: feature-specific → global config → error (no provider)
- [ ] Verify backward compatibility — no config = all enabled with global defaults (AC: #5)
- [ ] Build admin UI page for AI feature configuration (AC: #6)
  - [ ] Table with rows per operation type, columns for enabled/provider/model
  - [ ] Provider dropdown populated from configured providers
- [ ] Write integration tests (AC: #7)

## Dev Notes

### Architecture

- AI host function: `crates/kernel/src/host/ai.rs` — add feature config check before provider dispatch
- Site config: extend AI config schema to include per-feature overrides
- The existing AI provider config becomes the *default*; per-feature config overrides it
- Admin route: add to appropriate `admin_ai_*.rs` module

### Security

- Only admin users can modify AI feature configuration
- Disabled features return clear error, not silent failure

### Testing

- Integration tests: use `#[test]` + `run_test(async { ... })` on `SHARED_RT` runtime
- Test disabled feature returns error
- Test per-feature provider override applies correct provider

### References

- [Source: docs/ritrovo/epic-17-external.md — Story 47.6]
- [Source: crates/kernel/src/host/ai.rs] — AI request dispatch
- [Source: docs/design/ai-integration.md] — AI architecture
