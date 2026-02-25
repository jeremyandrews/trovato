# Story 31.3: Token Budget Management

Status: done

## Story

As a **site administrator**,
I want to **set token usage limits per role and provider**,
so that **AI costs are predictable and controllable**.

## Acceptance Criteria

1. Token usage tracked per request from provider response metadata (prompt tokens + completion tokens)
2. Usage stored per-vendor, per-user with configurable period (daily, weekly, monthly)
3. Per-role default budgets configurable in site config (e.g., authenticated: 10K tokens/month, editor: 50K, admin: unlimited)
4. Per-user override stored in user record (`data` JSONB), editable in admin UI
5. Budget enforcement: `deny` (reject request), `warn` (allow but log warning), `queue` (defer for later — future Story, not implemented here)
6. `ai_request()` checks budget before making provider call; returns budget-exceeded error when `deny` is active
7. Admin UI: usage dashboard showing burn-down by provider, role, and user over the configured period
8. Usage log feeds the dashboard (persisted to `ai_usage_log` table, not just tracing)
9. Budget resets automatically at the start of each period

## Tasks / Subtasks

- [x] Task 1: Create `ai_usage_log` migration (AC: #1, #8)
  - [x] 1.1 Create `crates/kernel/migrations/20260226000003_create_ai_usage_log.sql`
  - [x] 1.2 Table: `ai_usage_log` with columns: id (UUID PK), user_id (UUID, nullable), plugin_name, provider_id, operation, model, prompt_tokens, completion_tokens, total_tokens, latency_ms, created (bigint unix timestamp)
  - [x] 1.3 Indexes: `created`, `user_id`, `provider_id`, composite `(provider_id, created)` for budget queries

- [x] Task 2: Create `AiTokenBudgetService` (AC: #2, #3, #4, #9)
  - [x] 2.1 Create `crates/kernel/src/services/ai_token_budget.rs`
  - [x] 2.2 Define `BudgetConfig` struct: `period` (daily/weekly/monthly), `action_on_limit` (deny/warn), `defaults` (HashMap of provider_id → role → limit)
  - [x] 2.3 Define `BudgetPeriod` enum: `Daily`, `Weekly`, `Monthly` — with method `period_start() -> i64` that returns unix timestamp of current period start
  - [x] 2.4 Implement `get_budget_config()` — reads from site_config key `"ai_token_budgets"`
  - [x] 2.5 Implement `save_budget_config()` — writes to site_config key `"ai_token_budgets"`
  - [x] 2.6 Implement `get_user_budget_override(pool, user_id, provider_id)` — reads from `users.data["ai_budget_overrides"][provider_id]`
  - [x] 2.7 Implement `set_user_budget_override(pool, user_id, provider_id, limit)` — writes to `users.data`
  - [x] 2.8 Implement `record_usage(pool, log_entry)` — INSERT into `ai_usage_log`
  - [x] 2.9 Implement `get_usage_for_period(pool, user_id, provider_id, since)` — SUM(total_tokens) from `ai_usage_log` WHERE user_id AND provider_id AND created >= since
  - [x] 2.10 Implement `check_budget(pool, user_id, roles, provider_id)` — resolves effective limit (per-user override > per-role default > unlimited), queries current usage, returns `BudgetCheckResult { allowed, remaining, limit, used, action }`

- [x] Task 3: Wire service into AppState (AC: #2)
  - [x] 3.1 Add `pub mod ai_token_budget;` to `crates/kernel/src/services/mod.rs`
  - [x] 3.2 Add `ai_budgets: Arc<AiTokenBudgetService>` to `AppStateInner` in `state.rs`
  - [x] 3.3 Initialize in `AppState::new()` and add getter method
  - [x] 3.4 Add `ai_budgets: Option<Arc<AiTokenBudgetService>>` to `RequestServices` in `request_state.rs`
  - [x] 3.5 Update `RequestServices` construction in `state.rs` and `for_background()` in `request_state.rs`

- [x] Task 4: Integrate into `ai_request()` host function (AC: #1, #6)
  - [x] 4.1 Add new error code `ERR_AI_BUDGET_EXCEEDED = -26` to `crates/plugin-sdk/src/host_errors.rs`
  - [x] 4.2 In `host/ai.rs`: after rate limit check (line ~452), add budget check using `services.ai_budgets`
  - [x] 4.3 If budget check returns `deny` and limit exceeded, return `ERR_AI_BUDGET_EXCEEDED`
  - [x] 4.4 If budget check returns `warn` and limit exceeded, log warning at `warn!` level but proceed
  - [x] 4.5 After successful response parsing (line ~504), call `record_usage()` to persist to `ai_usage_log`
  - [x] 4.6 Pass `caller.data().request.user.id` as `user_id` for the log entry

- [x] Task 5: Admin routes for budget configuration (AC: #3, #4, #7)
  - [x] 5.1 Create `crates/kernel/src/routes/admin_ai_budget.rs`
  - [x] 5.2 GET `/admin/system/ai-budgets` — usage dashboard: period usage by provider, top users, budget config form
  - [x] 5.3 POST `/admin/system/ai-budgets` — save budget config (period, action_on_limit, per-role defaults)
  - [x] 5.4 GET `/admin/system/ai-budgets/user/{user_id}` — per-user usage detail + override form
  - [x] 5.5 POST `/admin/system/ai-budgets/user/{user_id}` — save per-user budget override
  - [x] 5.6 Register routes: `pub mod admin_ai_budget;` in `routes/mod.rs`, merge in `admin.rs`

- [x] Task 6: Admin templates (AC: #7)
  - [x] 6.1 Create `templates/admin/ai-budgets.html` — usage dashboard with period stats table, budget config form
  - [x] 6.2 Create `templates/admin/ai-budget-user.html` — per-user usage detail + override form
  - [x] 6.3 Add sidebar link in `templates/page--admin.html` under AI Providers link

- [x] Task 7: Unit and integration tests (AC: all)
  - [x] 7.1 Unit tests for `BudgetPeriod::period_start()` — daily/weekly/monthly boundary calculations
  - [x] 7.2 Unit tests for budget resolution: per-user override > per-role default > unlimited
  - [x] 7.3 Integration test: record usage, check budget, verify enforcement
  - [x] 7.4 Integration test: budget resets at period boundary
  - [x] 7.5 Integration test: admin dashboard renders with usage data
  - [x] 7.6 Add `AI_BUDGET_LOCK: Mutex<()>` to test common for state isolation

- [x] Task 8: Verify (AC: all)
  - [x] 8.1 `cargo fmt --all`
  - [x] 8.2 `cargo clippy --all-targets -- -D warnings`
  - [x] 8.3 `cargo test --all`

## Dev Notes

### Architecture

This story implements **D3 (Granular token budget system)** from `docs/design/ai-integration.md`. Token budgets are a kernel-level concern (not plugin-gated) because they apply uniformly across all AI usage regardless of which plugin initiates the request. The service follows the same always-initialized pattern as `AiProviderService` and `SearchService`.

**Key design decisions from D3:**
- Budgets are per-vendor because tokens differ across providers (OpenAI tokens != Anthropic tokens)
- Per-role defaults keep config manageable; per-user overrides handle exceptions
- `action_on_limit` gives flexibility: hard deny vs soft warning
- The `queue` action is spec'd in the AC but should NOT be implemented in this story — just define the enum variant and return `deny` behavior for it. Queueing requires the queue worker infrastructure from Epic 13 and will be a follow-up.

### Data Types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    Deny,
    Warn,
    Queue, // Not implemented in this story — treated as Deny
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub period: BudgetPeriod,
    pub action_on_limit: BudgetAction,
    /// provider_id → role_name → token_limit (0 = unlimited)
    pub defaults: HashMap<String, HashMap<String, u64>>,
}

pub struct BudgetCheckResult {
    pub allowed: bool,
    pub remaining: Option<u64>,  // None if unlimited
    pub limit: u64,              // 0 = unlimited
    pub used: u64,
    pub action: BudgetAction,
}
```

### Storage

**Site config key `"ai_token_budgets"`** — stores `BudgetConfig` as JSONB:
```json
{
  "period": "monthly",
  "action_on_limit": "deny",
  "defaults": {
    "openai-main": {
      "authenticated": 10000,
      "editor": 50000,
      "admin": 0
    },
    "anthropic-main": {
      "authenticated": 10000,
      "editor": 50000,
      "admin": 0
    }
  }
}
```

**Per-user overrides** in `users.data` JSONB:
```json
{
  "ai_budget_overrides": {
    "openai-main": 100000,
    "anthropic-main": 75000
  }
}
```

**`ai_usage_log` table** — persistent usage records:
```sql
CREATE TABLE IF NOT EXISTS ai_usage_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID,
    plugin_name VARCHAR(255) NOT NULL,
    provider_id VARCHAR(255) NOT NULL,
    operation VARCHAR(64) NOT NULL,
    model VARCHAR(255) NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    latency_ms BIGINT NOT NULL DEFAULT 0,
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint
);
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_created ON ai_usage_log (created);
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_user_id ON ai_usage_log (user_id);
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_provider ON ai_usage_log (provider_id);
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_budget ON ai_usage_log (provider_id, created);
```

Migration file: `crates/kernel/migrations/20260226000003_create_ai_usage_log.sql`. Follow the `created BIGINT` convention (unix timestamp) — same as `url_alias.created` and `audit_log.created`.

### Budget Check Algorithm

```
1. Load BudgetConfig from site_config "ai_token_budgets"
   - If missing/empty → all budgets unlimited, always allow
2. Compute period_start timestamp for current period
3. Check per-user override: users.data["ai_budget_overrides"][provider_id]
   - If present → use as limit
4. If no user override, check per-role defaults:
   - For each role the user has, find defaults[provider_id][role]
   - Use the HIGHEST limit among all roles (most permissive wins)
   - If any role has limit 0 → unlimited
5. If limit is 0 (unlimited) → allow
6. Query: SELECT COALESCE(SUM(total_tokens), 0) FROM ai_usage_log
          WHERE user_id = $1 AND provider_id = $2 AND created >= $3
7. If used >= limit → return BudgetCheckResult { allowed: false, action: config.action_on_limit }
8. If used < limit → return BudgetCheckResult { allowed: true, remaining: limit - used }
```

### Period Start Calculation

```rust
impl BudgetPeriod {
    pub fn period_start(&self) -> i64 {
        let now = chrono::Utc::now();
        match self {
            BudgetPeriod::Daily => now.date_naive().and_hms_opt(0, 0, 0)
                .unwrap().and_utc().timestamp(),
            BudgetPeriod::Weekly => {
                // Monday 00:00 UTC of current week
                let days_since_monday = now.weekday().num_days_from_monday();
                (now - chrono::Duration::days(days_since_monday as i64))
                    .date_naive().and_hms_opt(0, 0, 0)
                    .unwrap().and_utc().timestamp()
            }
            BudgetPeriod::Monthly => now.with_day(1).unwrap()
                .date_naive().and_hms_opt(0, 0, 0)
                .unwrap().and_utc().timestamp(),
        }
    }
}
```

Use `chrono` (already a kernel dependency) for date calculations.

### Host Function Integration Point

In `crates/kernel/src/host/ai.rs`, the budget check goes **after** the rate limit check (line ~452) and **before** `let started = Instant::now()` (line ~454):

```rust
// Check rate limit (existing, line 444-452)
if !check_rate_limit(&resolved.config.id, resolved.config.rate_limit_rpm) { ... }

// NEW: Check token budget
if let Some(ref budget_svc) = services.ai_budgets {
    let user_id = caller.data().request.user.id;
    let user_roles = &caller.data().request.user.permissions; // roles from UserContext
    match budget_svc.check_budget(&services.db, user_id, user_roles, &resolved.config.id).await {
        Ok(result) if !result.allowed => {
            match result.action {
                BudgetAction::Deny | BudgetAction::Queue => {
                    warn!(plugin = %plugin_name, provider = %resolved.config.id,
                          used = result.used, limit = result.limit,
                          "AI token budget exceeded (deny)");
                    return host_errors::ERR_AI_BUDGET_EXCEEDED;
                }
                BudgetAction::Warn => {
                    warn!(plugin = %plugin_name, provider = %resolved.config.id,
                          used = result.used, limit = result.limit,
                          "AI token budget exceeded (warn, allowing)");
                    // Fall through — allow the request
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "budget check failed, allowing request");
            // Fail-open: if budget service errors, allow the request
        }
        _ => {} // Budget OK
    }
}

let started = Instant::now(); // existing line 454
```

After the successful response (line ~504), persist the usage:

```rust
// NEW: Record usage to ai_usage_log
if let Some(ref budget_svc) = services.ai_budgets {
    let user_id = caller.data().request.user.id;
    if let Err(e) = budget_svc.record_usage(&services.db, UsageLogEntry {
        user_id: if user_id.is_nil() { None } else { Some(user_id) },
        plugin_name: plugin_name.clone(),
        provider_id: resolved.config.id.clone(),
        operation: kernel_op.to_string(),
        model: ai_response.model.clone(),
        prompt_tokens: ai_response.usage.prompt_tokens as i32,
        completion_tokens: ai_response.usage.completion_tokens as i32,
        total_tokens: ai_response.usage.total_tokens as i32,
        latency_ms: latency_ms as i64,
    }).await {
        warn!(error = %e, "failed to record AI usage log");
        // Non-fatal: don't fail the request if logging fails
    }
}
```

**Important:** The `caller.data()` borrow must not be held across `.await` points. Clone `user_id`, `plugin_name`, and `permissions` before the budget check `await`. The existing code already clones `plugin_name` at line 385 and has `ai_svc.clone()` at line 384. Follow this same pattern for `user_id` and roles.

### Role Resolution

The `UserContext` (in `request_state.rs`) has `permissions: Vec<String>` — these are permission strings, NOT role names. For budget purposes, you need role names. Two approaches:

**Approach A (Recommended):** Budget defaults keyed by permission string instead of role name. E.g., `defaults["openai"]["use ai chat"] = 50000`. This avoids needing a separate role→name mapping.

**Approach B:** Add role names to `UserContext`. The user's roles are loaded in `load_user_roles()` at `auth.rs`. You'd need to extend `UserContext` with `pub roles: Vec<String>` and populate it during session extraction.

**Go with Approach A** — it avoids touching the auth layer and aligns with how Trovato already works (permissions, not roles, are the authorization primitive).

**WAIT — re-read the acceptance criteria**: "Per-role default budgets configurable in site config (e.g., authenticated: 10K tokens/month, editor: 50K, admin: unlimited)". These are ROLE names. The admin UI should list roles and let the admin set budgets per role. The budget check resolves: user's roles → find matching budget entries → use highest. This means we DO need role names in the budget check.

**Resolution:** The `User` model doesn't currently carry role names through to `UserContext`. However, `UserContext::permissions` contains the aggregated permission list. We can map role budgets by loading the user's role names from the DB at budget check time: `SELECT r.name FROM role r JOIN user_role ur ON r.id = ur.role_id WHERE ur.user_id = $1`. This is a single indexed query, called only when budget enforcement is active. Cache in the `BudgetCheckResult` if needed.

Alternatively, add `roles: Vec<String>` to `UserContext` — it's populated from the same `load_user_roles` query that already loads permissions. This is cleaner. Add the field and populate it in `build_user_context()` in `routes/helpers.rs` (or wherever `UserContext::authenticated()` is called).

### Service Structure

```rust
pub struct AiTokenBudgetService {
    db: PgPool,
}

impl AiTokenBudgetService {
    pub fn new(db: PgPool) -> Self { Self { db } }

    pub async fn get_config(&self) -> Result<Option<BudgetConfig>> { ... }
    pub async fn save_config(&self, config: &BudgetConfig) -> Result<()> { ... }
    pub async fn record_usage(&self, pool: &PgPool, entry: UsageLogEntry) -> Result<()> { ... }
    pub async fn get_usage_for_period(&self, pool: &PgPool, user_id: Uuid, provider_id: &str, since: i64) -> Result<u64> { ... }
    pub async fn check_budget(&self, pool: &PgPool, user_id: Uuid, roles: &[String], provider_id: &str) -> Result<BudgetCheckResult> { ... }
    pub async fn get_user_override(&self, pool: &PgPool, user_id: Uuid, provider_id: &str) -> Result<Option<u64>> { ... }
    pub async fn set_user_override(&self, pool: &PgPool, user_id: Uuid, provider_id: &str, limit: u64) -> Result<()> { ... }

    // Dashboard queries
    pub async fn usage_by_provider(&self, since: i64) -> Result<Vec<ProviderUsageSummary>> { ... }
    pub async fn usage_by_user(&self, since: i64, limit: i64) -> Result<Vec<UserUsageSummary>> { ... }
    pub async fn usage_by_role(&self, since: i64) -> Result<Vec<RoleUsageSummary>> { ... }
}
```

Note: `record_usage` and `get_usage_for_period` take `pool: &PgPool` as a parameter (not `&self.db`) so they can be called from the host function with the request-scoped pool. The service also has its own `self.db` for config operations and dashboard queries.

### Admin Routes

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | `/admin/system/ai-budgets` | `budget_dashboard` | Usage stats + budget config form |
| POST | `/admin/system/ai-budgets` | `save_budget_config` | Save period, action, per-role defaults |
| GET | `/admin/system/ai-budgets/user/{id}` | `user_budget_detail` | Per-user usage + override form |
| POST | `/admin/system/ai-budgets/user/{id}` | `save_user_override` | Save per-user budget override |

All handlers: `require_admin()` + `require_csrf()` on POST. Follow `admin_ai_provider.rs` patterns exactly.

### Admin Sidebar

Add after AI Providers link in `templates/page--admin.html`:
```html
<li><a href="/admin/system/ai-budgets" {% if path is starting_with("/admin/system/ai-budgets") %}class="active"{% endif %}>AI Budgets</a></li>
```

### Dashboard Template

The dashboard page shows:
1. **Period selector** — current period (e.g., "February 2026") with the configured period type
2. **Usage by provider** — table: provider name, total tokens used, budget (sum of all role defaults)
3. **Top users** — table: username, provider, tokens used, budget, % used
4. **Budget configuration form** — period dropdown, action_on_limit dropdown, per-provider per-role limits grid
5. **Link to per-user detail** for each user in the top users table

Use `.admin-card` for each section. Use `<table class="table">` for data tables. Follow the CSS patterns from `ai-providers.html`.

### Error Code

Add to `crates/plugin-sdk/src/host_errors.rs`:
```rust
/// Token budget exceeded for the current period.
pub const ERR_AI_BUDGET_EXCEEDED: i32 = -26;
```

Add to the doc table in the AI API section.

### Security

- Budget configuration requires admin permission — same as AI provider config
- Per-user overrides editable only by admins
- Usage data contains user IDs — admin-only access
- The `ai_usage_log` table stores plugin_name and user_id for audit but no sensitive content (no prompts, no responses)
- Budget check is fail-open: if the budget service errors, allow the request (don't block AI operations due to budget DB issues)

### What NOT to Do

- Do NOT implement the `queue` action — define the enum variant but treat it as `deny`. Queue infrastructure is out of scope.
- Do NOT store prompts or responses in `ai_usage_log` — only metadata (tokens, model, latency)
- Do NOT add streaming support — that's Story 31.5 (chatbot)
- Do NOT add permission checks in this story — that's Story 31.4
- Do NOT create a separate migration for user data schema changes — the `data` JSONB column already exists and is schemaless
- Do NOT use `format!()` for SQL — use sqlx parameterized queries
- Do NOT create local copies of `html_escape`, `require_csrf`, `render_error`, etc. — import from `crate::routes::helpers`
- Do NOT use `#[tokio::test]` — use `#[test]` + `run_test(async { ... })` with `SHARED_RT`

### Project Structure Notes

- Service file: `crates/kernel/src/services/ai_token_budget.rs` — new file
- Admin routes: `crates/kernel/src/routes/admin_ai_budget.rs` — new file
- Migration: `crates/kernel/migrations/20260226000003_create_ai_usage_log.sql` — new file
- Templates: `templates/admin/ai-budgets.html`, `templates/admin/ai-budget-user.html` — new files
- Modified: `host/ai.rs`, `state.rs`, `request_state.rs`, `services/mod.rs`, `routes/mod.rs`, `routes/admin.rs`, `page--admin.html`, `host_errors.rs`

### References

- [Source: docs/design/ai-integration.md#D3] — Granular token budget system design
- [Source: docs/ritrovo/epic-03.md#Story 31.3] — Epic acceptance criteria (lines 151-169)
- [Source: crates/kernel/src/host/ai.rs:444-504] — Rate limit check + logging (budget insertion points)
- [Source: crates/kernel/src/services/ai_provider.rs:290-310] — AiProviderService pattern to follow
- [Source: crates/kernel/src/models/site_config.rs:22-50] — SiteConfig::get/set pattern
- [Source: crates/kernel/src/models/user.rs:30] — User.data JSONB column for per-user overrides
- [Source: crates/kernel/src/tap/request_state.rs:62-83] — RequestServices struct to extend
- [Source: crates/kernel/src/state.rs:108,419,759] — AppState service wiring pattern
- [Source: crates/plugin-sdk/src/host_errors.rs] — Error code conventions (AI codes -20 to -25, new -26)
- [Source: templates/admin/ai-providers.html] — Admin template pattern for AI pages
- [Source: _bmad-output/implementation-artifacts/31-2-wasm-ai-host-functions.md] — Previous story intelligence

## Previous Story Intelligence

### Story 31.2 Learnings

Story 31.2 (`ai_request()` host function) was completed and passed adversarial review. Key patterns established:

1. **`func_wrap_async` pattern** — All async host functions use `Box::new(async move { ... })`. The `caller.data()` borrow must NOT be held across `.await` points — clone everything needed before any await.
2. **Services access** — `caller.data().request.services()` returns `Option<&RequestServices>`. The `ai_providers` field is `Option<Arc<AiProviderService>>`. Clone the Arc before awaiting.
3. **Rate limiter** — In-memory `LazyLock<Mutex<HashMap<...>>>` with 60-second windows and stale entry eviction. Budget checking should NOT use this pattern (budgets need persistent storage).
4. **Error codes** — AI codes are -20 through -25. The new budget code is -26.
5. **`RequestServices::for_background()`** — Already updated to accept `ai_providers`. Must also accept the new `ai_budgets` field.
6. **Serde coupling** — SDK `AiOperationType` and kernel `AiOperationType` are separate enums with identical `snake_case` serde representations. They interop via JSON string conversion.
7. **Adversarial findings addressed** — model override, safe UTF-8 truncation, multiple system message concatenation, content block concatenation, role validation, pre-check buffer overflow, stale rate limiter eviction.

### Files Created/Modified in 31.2

- `crates/kernel/src/host/ai.rs` (CREATED) — Host function implementation (~530 lines)
- `crates/kernel/src/host/mod.rs` (MODIFIED) — Registration
- `crates/kernel/src/tap/request_state.rs` (MODIFIED) — Added ai_providers to RequestServices
- `crates/kernel/src/services/ai_provider.rs` (MODIFIED) — Added http() getter
- `crates/kernel/src/cron/mod.rs` (MODIFIED) — Added ai_providers to CronService
- `crates/kernel/src/state.rs` (MODIFIED) — Wired ai_providers into cron
- `crates/plugin-sdk/src/types.rs` (MODIFIED) — Added AI types
- `crates/plugin-sdk/src/host_errors.rs` (MODIFIED) — Added AI error codes (-20 to -25), SDK codes (-100 to -102)
- `crates/plugin-sdk/src/host.rs` (MODIFIED) — Added AI host binding
- `crates/wit/kernel.wit` (MODIFIED) — Added ai-api interface

## Git Intelligence

Recent commits:
- `3e2cef9` fix: address adversarial review findings for Story 31-2
- `5394709` feat: add AI provider registry with admin UI (Story 31.1)
- `1f19a27` feat: add tested tutorial assertions for Part 1 (19 tests)
- `b86bc5b` feat: add conference gather at /conferences and align tutorial with epic

Conventions: imperative commit messages, `feat:` / `fix:` / `docs:` prefixes, story reference in message.

## Dev Agent Record

### Agent Model Used

{{agent_model_name_version}}

### Debug Log References

### Completion Notes List

### File List
