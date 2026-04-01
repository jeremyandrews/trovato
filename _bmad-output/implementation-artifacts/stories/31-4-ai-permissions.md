# Story 31.4: AI Permissions

Status: done

## Story

As a **site administrator**,
I want to control which roles can use AI features through the existing permission system,
so that AI usage is governed by the same role-based access model as all other Trovato features.

## Acceptance Criteria

1. **AC1: Permission Definitions** — Six AI permissions exist and appear in the admin role-permission matrix at `/admin/people/permissions`:
   - `use ai` — base permission required for any AI operation
   - `use ai chat` — use AI chat/completion operations
   - `use ai embeddings` — use AI embedding operations
   - `use ai image generation` — use AI image generation
   - `configure ai` — manage AI providers, field rules, and chat settings
   - `view ai usage` — view the AI token usage dashboard

2. **AC2: Host Function Enforcement** — `ai_request()` in `host/ai.rs` checks `use ai` (base) plus the operation-specific permission (e.g., `use ai chat` for `Chat` operations) BEFORE rate limit and budget checks. Returns `ERR_AI_PERMISSION_DENIED` (`-27`) on failure.

3. **AC3: Anonymous User Handling** — Anonymous users (no session / `UserContext::anonymous()`) who trigger `ai_request()` receive a clean "AI features require authentication" error distinguishable from "authenticated but lacks permission." The error code is the same (`-27`) but the logged message differentiates the two cases.

4. **AC4: Admin Route Protection — Providers** — All routes in `admin_ai_provider.rs` require `configure ai` permission instead of blanket `require_admin`. Admin users (with `is_admin` flag) are implicitly granted all permissions by `PermissionService`.

5. **AC5: Admin Route Protection — Budgets** — `admin_ai_budget.rs` routes: the budget dashboard requires `view ai usage`; save/override endpoints require `configure ai`.

6. **AC6: Permission-Aware Helper** — `require_permission(state, session, permission)` and `require_permission_json(state, session, permission)` helpers in `routes/helpers.rs` load the user, check the named permission via `PermissionService`, and return `Ok(User)` or an appropriate error response (redirect to login if unauthenticated, 403 if lacking permission). Reusable for all permission-gated routes.

7. **AC7: Integration Tests** — Tests verify: (a) non-admin with `configure ai` can access provider routes (200); (b) non-admin without `configure ai` gets 403; (c) non-admin with `view ai usage` can access budget dashboard (200); (d) unauthenticated user redirects to login (303); (e) admin implicitly has all AI permissions (200); (f) `permission_for_operation` unit test maps all 6 operation types.

## Dev Notes

### Key Implementation Details

- Permission check in `host/ai.rs` uses `UserContext::has_permission()` (Vec scan, synchronous — no await issue)
- `permission_for_operation()` maps: Chat -> "use ai chat", Embedding -> "use ai embeddings", ImageGeneration -> "use ai image generation", others -> "use ai" (base only)
- Permission check order: base `use ai` first, then operation-specific permission
- `require_permission` modeled after existing `require_admin` in `helpers.rs`
- `UserContext.authenticated` is `false` for anonymous users (id is `Uuid::nil()`)
- `AI_PERMISSION_LOCK: Mutex<()>` for integration test serialization

### Files

**Modified:**
- `crates/plugin-sdk/src/host_errors.rs` — Added `ERR_AI_PERMISSION_DENIED = -27`
- `crates/kernel/src/routes/helpers.rs` — Added `require_permission()` and `require_permission_json()`
- `crates/kernel/src/routes/admin_user.rs` — Extended `AVAILABLE_PERMISSIONS` with 6 AI permissions
- `crates/kernel/src/host/ai.rs` — Added `permission_for_operation()` + permission check block
- `crates/kernel/src/routes/admin_ai_provider.rs` — Replaced `require_admin` with `require_permission("configure ai")`
- `crates/kernel/src/routes/admin_ai_budget.rs` — Replaced `require_admin` with `require_permission("view ai usage"/"configure ai")`
- `crates/kernel/tests/integration_test.rs` — Added `AI_PERMISSION_LOCK` + 6 integration tests

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- `UserContext.id` is `Uuid` (not `Option<Uuid>`); anonymous detection uses `!user.authenticated`
- `require_permission_json` returns `Result<(), (StatusCode, Json<JsonError>)>` (user object not needed by JSON callers)
- Tests create roles, add permissions, assign to users via direct SQL, invalidate PermissionService cache
- Host function permission tests replaced with HTTP-level integration tests + unit test for mapping function

### File List

- `~ crates/plugin-sdk/src/host_errors.rs`
- `~ crates/kernel/src/routes/helpers.rs`
- `~ crates/kernel/src/routes/admin_user.rs`
- `~ crates/kernel/src/host/ai.rs`
- `~ crates/kernel/src/routes/admin_ai_provider.rs`
- `~ crates/kernel/src/routes/admin_ai_budget.rs`
- `~ crates/kernel/tests/integration_test.rs`
