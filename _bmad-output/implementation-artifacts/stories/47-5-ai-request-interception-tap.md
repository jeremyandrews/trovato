# Story 47.5: AI Request Interception Tap

Status: ready-for-dev

## Story

As a **plugin developer implementing AI governance**,
I want a `tap_ai_request` hook that fires before AI requests are sent,
so that plugins can inspect, modify, or deny AI requests based on policy.

## Acceptance Criteria

1. New `tap_ai_request` tap registered in the tap registry
2. The tap fires before the AI request is sent to the provider
3. Tap signature: `(request: &mut AiRequest, context: &AiRequestContext) -> AiRequestDecision`
4. `AiRequestContext` struct contains: `user_id`, `plugin_name`, `operation_type`, `item_id` (optional), `field_name` (optional)
5. `AiRequestDecision` enum: `Allow`, `AllowModified`, `Deny(reason: String)`
6. When multiple plugins implement the tap: first `Deny` wins (deny-wins aggregation)
7. Denied requests are logged in `ai_usage_log` with `status = "denied"` and the deny reason
8. At least 3 integration tests: allow passthrough, deny blocking, multi-plugin deny-wins aggregation

## Tasks / Subtasks

- [ ] Define `AiRequestContext` struct in `crates/plugin-sdk/src/types.rs` (AC: #4)
- [ ] Define `AiRequestDecision` enum in `crates/plugin-sdk/src/types.rs` (AC: #5)
- [ ] Register `tap_ai_request` in the tap registry (AC: #1)
- [ ] Implement tap invocation in `ai_request()` before provider dispatch (AC: #2)
- [ ] Pass `AiRequest` (mutable) and `AiRequestContext` to tap handlers (AC: #3)
- [ ] Implement deny-wins aggregation: iterate plugins, stop on first `Deny` (AC: #6)
- [ ] Log denied requests to `ai_usage_log` with `status = "denied"` and `deny_reason` (AC: #7)
- [ ] Add `deny_reason` column to `ai_usage_log` if not present (AC: #7)
- [ ] Write integration test: tap returns `Allow`, request proceeds normally (AC: #8)
- [ ] Write integration test: tap returns `Deny`, request is blocked and logged (AC: #8)
- [ ] Write integration test: two plugins, one allows, one denies; deny wins (AC: #8)

## Dev Notes

### Architecture

The tap follows the existing tap pattern in the registry. The `ai_request()` function collects decisions from all registered tap handlers. If any returns `Deny`, the request is immediately short-circuited with an error response indicating the denial reason. `AllowModified` indicates the plugin modified the request (e.g., changed the model, added system prompt constraints) but allows it to proceed.

The deny-wins aggregation means plugins cannot override another plugin's denial. This is the safe default for governance: any plugin can block, none can force-allow.

### Security

- The tap receives `&mut AiRequest`, allowing plugins to modify prompts. This is intentional for governance (e.g., injecting safety preambles) but means tap plugins are trusted.
- Deny reasons are logged but not exposed to end users (to prevent information leakage about governance rules).
- The `AiRequestContext` provides enough information for policy decisions without exposing raw content.

### Testing

- Register a test plugin that always returns `Allow`. Make an AI request. Verify it succeeds.
- Register a test plugin that returns `Deny("policy violation")`. Make an AI request. Verify it fails and the log entry has `status = "denied"`.
- Register two plugins: one returns `Allow`, one returns `Deny`. Verify the request is denied regardless of plugin ordering.

### References

- `crates/kernel/src/tap/` -- tap registry and invocation
- `crates/plugin-sdk/src/types.rs` -- SDK type definitions
- `crates/kernel/src/host/ai.rs` -- AI request dispatch
- Story 47.4 for `ai_usage_log` schema
