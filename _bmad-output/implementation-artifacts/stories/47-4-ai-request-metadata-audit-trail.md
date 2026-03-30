# Story 47.4: AI Request Metadata Audit Trail

Status: ready-for-dev

## Story

As a **site operator monitoring AI usage**,
I want metadata about every AI request logged,
so that I can track usage patterns, costs, and errors without storing sensitive prompt content.

## Acceptance Criteria

1. Every `ai_request()` call logs metadata to an `ai_usage_log` table
2. Logged fields: `model`, `operation_type`, `input_tokens`, `output_tokens`, `latency_ms`, `user_id`, `plugin_name`, `finish_reason`, `status` (success/error/timeout/denied)
3. Verify that token counts are already being logged; add any missing columns via migration
4. Metadata logging is always on (audit infrastructure, not optional)
5. Actual prompt and response content are NOT logged (privacy); content logging is plugin territory via `tap_ai_request`
6. `GET /admin/reports/ai-usage` shows aggregate stats (total requests, tokens by model, error rate, top operation types)
7. At least 2 integration tests: metadata logging on AI request, admin report endpoint

## Tasks / Subtasks

- [ ] Audit existing `ai_usage_log` table schema for missing columns (AC: #2, #3)
- [ ] Write migration to add any missing columns (`latency_ms`, `finish_reason`, `status`, `plugin_name`, `operation_type`, etc.) (AC: #2, #3)
- [ ] Update `ai_request()` to record all metadata fields on every call (AC: #1, #4)
- [ ] Ensure prompt/response content fields are NOT included in the log (AC: #5)
- [ ] Add `status` field tracking: success, error, timeout, denied (AC: #2)
- [ ] Create `GET /admin/reports/ai-usage` route handler (AC: #6)
- [ ] Build admin report template showing aggregate stats: total requests, tokens by model, error rates, top operations (AC: #6)
- [ ] Write integration test: make an AI request, verify metadata row is created with correct fields (AC: #7)
- [ ] Write integration test: `GET /admin/reports/ai-usage` returns valid page with stats (AC: #7)

## Dev Notes

### Architecture

The `ai_usage_log` table may already exist with some fields. This story audits what exists and adds missing columns. The logging call happens in the `ai_request()` function after the provider returns (or on error/timeout), capturing latency by measuring elapsed time.

The admin report aggregates with SQL: `COUNT(*)`, `SUM(input_tokens)`, `SUM(output_tokens)`, grouped by model, operation_type, status, and time period (last 24h, 7d, 30d).

### Security

- The admin report endpoint requires admin authentication.
- No prompt or response content is stored. This is a deliberate privacy boundary -- content logging, if desired, is implemented by plugins via `tap_ai_request` (Story 47.5).
- `user_id` is logged for accountability but the report shows aggregates, not per-user details (unless admin drills down).

### Testing

- Mock or use a test AI provider. Make a request. Query `ai_usage_log` and verify the row contains model, operation_type, tokens, latency_ms, and status.
- Hit `/admin/reports/ai-usage` as an admin user. Verify the page renders with aggregate statistics.

### References

- `crates/kernel/src/host/ai.rs` -- AI request implementation
- `crates/kernel/src/routes/admin.rs` -- admin route handlers
- Existing `ai_usage_log` table (audit for current schema)
