# Story 31.3: Token Budget Tracking

Status: done

## Story

As a **site administrator**,
I want to set token usage limits per role and provider,
so that AI costs are predictable and controllable.

## Acceptance Criteria

1. **AC1: Usage Logging** — Token usage tracked per request from provider response metadata (prompt_tokens + completion_tokens). Persisted to `ai_usage_log` table with columns: id (UUID), user_id, plugin_name, provider_id, operation, model, prompt_tokens, completion_tokens, total_tokens, latency_ms, created (bigint unix timestamp).

2. **AC2: Period-Based Tracking** — Usage aggregated per-vendor, per-user with configurable period: daily (resets 00:00 UTC), weekly (resets Monday 00:00 UTC), monthly (resets 1st of month 00:00 UTC).

3. **AC3: Per-Role Default Budgets** — Role-based budgets configurable in site config under key `"ai_token_budgets"`. Example: authenticated role gets 10K tokens/month, editor gets 50K, admin unlimited.

4. **AC4: Per-User Overrides** — Per-user budget overrides stored in user record's `data` JSONB column at `ai_budget_overrides[provider_id]`. Editable in admin UI at `/admin/system/ai-budgets/{user_id}`.

5. **AC5: Budget Resolution** — Resolution order: (1) per-user override, (2) per-role default (highest limit among user's roles wins — most permissive), (3) no config means unlimited.

6. **AC6: Enforcement Actions** — Three enforcement modes: `deny` (reject request with `ERR_AI_BUDGET_EXCEEDED`), `warn` (allow but log warning), `queue` (deferred — not implemented in this story).

7. **AC7: Host Function Integration** — `ai_request()` checks budget BEFORE making the provider HTTP call. Returns `ERR_AI_BUDGET_EXCEEDED` (-26) when enforcement action is `deny` and budget is exceeded.

8. **AC8: Admin Dashboard** — Usage dashboard at `/admin/system/ai-budgets` showing burn-down by provider, role, and user over the configured period. Per-user detail page shows individual usage and override controls.

9. **AC9: Auto-Reset** — Budget usage resets automatically at the start of each period by querying usage only within the current period window (no explicit reset operation needed).

## Dev Notes

### Key Implementation Details

- `AiTokenBudgetService` (722 lines) in `crates/kernel/src/services/ai_token_budget.rs`
- `BudgetPeriod` enum: `Daily`, `Weekly`, `Monthly` with `period_start()` computing UTC boundary timestamps
- `BudgetConfig` stored in `site_config` under key `"ai_token_budgets"` — maps role names to per-provider budget limits
- `check_budget()` returns `BudgetCheckResult { allowed, remaining, limit, used, action }` — called by `host/ai.rs` before HTTP request
- `record_usage()` inserts into `ai_usage_log` after provider response — called by `host/ai.rs` after successful request
- `UsageLogEntry`: user_id, plugin_name, provider_id, operation, model, prompt_tokens, completion_tokens, total_tokens, latency_ms
- Per-user overrides use atomic JSONB operations on `users.data` column
- Saturating `u32 -> i32` conversion for token counts prevents overflow
- `.max(0)` guard on SQL aggregate sums prevents negative values

### Database

- Migration: `crates/kernel/migrations/20260226000003_create_ai_usage_log.sql`
- Table: `ai_usage_log` with indexes on `created`, `user_id`, `provider_id`, composite `(provider_id, created)`
- Budget config: `site_config` table (no dedicated budget table)
- Per-user overrides: `users.data` JSONB column

### Files

**Created:**
- `crates/kernel/src/services/ai_token_budget.rs` — AiTokenBudgetService, budget types, check/record logic
- `crates/kernel/src/routes/admin_ai_budget.rs` — Admin dashboard and per-user budget routes
- `crates/kernel/migrations/20260226000003_create_ai_usage_log.sql` — ai_usage_log table
- `templates/admin/ai-budgets.html` — Budget dashboard template
- `templates/admin/ai-budget-user.html` — Per-user budget detail template

**Modified:**
- `crates/kernel/src/services/mod.rs` — Added `pub mod ai_token_budget;`
- `crates/kernel/src/state.rs` — Added ai_budgets field + getter to AppStateInner
- `crates/kernel/src/routes/mod.rs` — Added `pub mod admin_ai_budget;`, merged router
- `crates/kernel/src/host/ai.rs` — Added budget check before HTTP call, usage recording after response
- `templates/page--admin.html` — Added "AI Budgets" sidebar link under System section

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Budget enforcement inserted between rate limit check and HTTP call in `host/ai.rs`
- Usage recording inserted after successful provider response
- `AI_BUDGET_LOCK: Mutex<()>` added for integration test serialization
- Period auto-reset works by querying `WHERE created >= period_start` — no cron job needed
- Adversarial review caught: saturating u32-to-i32 conversion, .max(0) on aggregates, composite index

### File List

- `crates/kernel/src/services/ai_token_budget.rs` (722 lines)
- `crates/kernel/src/routes/admin_ai_budget.rs` (451 lines)
- `crates/kernel/migrations/20260226000003_create_ai_usage_log.sql`
- `templates/admin/ai-budgets.html`
- `templates/admin/ai-budget-user.html`
