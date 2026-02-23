# Story 31.4: AI Permissions

Status: done

## Story

As a site administrator,
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

6. **AC6: Permission-Aware Helper** — A new `require_permission(state, session, permission)` helper in `routes/helpers.rs` loads the user, checks the named permission via `PermissionService`, and returns `Ok(User)` or an appropriate error response (redirect to login if unauthenticated, 403 if lacking permission). This helper is reusable for all future permission-gated admin routes.

7. **AC7: Integration Tests** — Tests verify: (a) user WITH `use ai` + `use ai chat` can trigger chat operations; (b) user WITHOUT the permission gets `-27`; (c) anonymous user gets `-27`; (d) non-admin with `configure ai` can access provider admin routes; (e) non-admin without `configure ai` gets 403 on provider routes.

## Tasks / Subtasks

- [x] Task 1: Add `ERR_AI_PERMISSION_DENIED` constant (AC: #2)
  - [x] 1.1 Add `pub const ERR_AI_PERMISSION_DENIED: i32 = -27;` to `crates/plugin-sdk/src/host_errors.rs`
  - [x] 1.2 Add `-27` entry to the AI API doc table in the same file
- [x] Task 2: Add `require_permission` helper (AC: #6)
  - [x] 2.1 Add `require_permission(state, session, permission) -> Result<User, Response>` to `crates/kernel/src/routes/helpers.rs`
  - [x] 2.2 Add `require_permission_json(state, session, permission) -> Result<(), (StatusCode, Json<JsonError>)>` variant for JSON endpoints
- [x] Task 3: Add AI permissions to `AVAILABLE_PERMISSIONS` (AC: #1)
  - [x] 3.1 Extend `AVAILABLE_PERMISSIONS` in `crates/kernel/src/routes/admin_user.rs` with all 6 AI permission strings
- [x] Task 4: Enforce permissions in `ai_request()` host function (AC: #2, #3)
  - [x] 4.1 After request deserialization + validation, check `use ai` base permission
  - [x] 4.2 Map `AiOperationType` to operation-specific permission string and check it
  - [x] 4.3 Return `ERR_AI_PERMISSION_DENIED` on failure with descriptive log message
  - [x] 4.4 For anonymous users, log "AI features require authentication" vs "User {id} lacks permission {perm}"
- [x] Task 5: Replace `require_admin` in provider routes (AC: #4)
  - [x] 5.1 Replace all `require_admin` calls in `admin_ai_provider.rs` with `require_permission(state, session, "configure ai")`
  - [x] 5.2 Replace `require_admin_json` in `test_provider` with `require_permission_json`
- [x] Task 6: Replace `require_admin` in budget routes (AC: #5)
  - [x] 6.1 `budget_dashboard` → `require_permission(state, session, "view ai usage")`
  - [x] 6.2 `save_budget_config`, `user_budget_detail`, `save_user_override` → `require_permission(state, session, "configure ai")`
- [x] Task 7: Integration tests (AC: #7)
  - [x] 7.1 Test: non-admin with `configure ai` can GET `/admin/system/ai-providers` (200)
  - [x] 7.2 Test: non-admin without `configure ai` gets 403 on `/admin/system/ai-providers`
  - [x] 7.3 Test: non-admin with `view ai usage` can GET `/admin/system/ai-budgets` (200)
  - [x] 7.4 Test: unauthenticated user redirects to `/user/login` (303)
  - [x] 7.5 Test: admin implicitly has all AI permissions (200 on both pages)
  - [x] 7.6 Unit test: `permission_for_operation` maps all 6 operation types correctly
- [x] Task 8: Verify (`cargo fmt`, `cargo clippy`, `cargo test --all`)

## Dev Notes

### Permission System Architecture

Trovato has two permission registries:
1. **Kernel-hardcoded `AVAILABLE_PERMISSIONS`** in `admin_user.rs` (line ~24) — drives the admin permissions matrix UI at `/admin/people/permissions`
2. **Plugin-declared permissions** via `tap_perm` — `PermissionDefinition` in `crates/plugin-sdk/src/types.rs`

For Story 31.4, add AI permissions to `AVAILABLE_PERMISSIONS` directly. The `trovato_ai` plugin (future story) will also declare them via `tap_perm`, but the kernel constant is what the admin UI iterates.

**Permission checking has two code paths:**
- **Route handlers** (kernel-side): Use `PermissionService::user_has_permission(&user, "perm")` which loads from `role_permissions` table with `DashMap` cache. Admin users (`is_admin == true`) always return `true`.
- **WASM host functions**: Use `UserContext::has_permission("perm")` which does a linear scan of the pre-loaded `Vec<String>` permissions in `request_state.rs`.

The AI permission check in `host/ai.rs` uses the **WASM path** (`caller.data().request.user.has_permission()`).

### `require_permission` Helper Pattern

Model after the existing `require_admin` in `helpers.rs` (line ~70):

```rust
pub async fn require_permission(
    state: &AppState,
    session: &Session,
    permission: &str,
) -> Result<User, Response> {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    if let Some(id) = user_id
        && let Ok(Some(user)) = User::find_by_id(state.db(), id).await
    {
        if !user.is_active() {
            let _ = session.delete().await;
            return Err(Redirect::to("/user/login").into_response());
        }
        // Admin users implicitly have all permissions
        if user.is_admin {
            return Ok(user);
        }
        if state.permissions().user_has_permission(&user, permission).await.unwrap_or(false) {
            return Ok(user);
        }
        return Err((StatusCode::FORBIDDEN, Html("Access denied")).into_response());
    }
    Err(Redirect::to("/user/login").into_response())
}
```

Also create `require_permission_json` for JSON API endpoints (returns JSON error instead of HTML/redirect), following the pattern of `require_admin_json`.

### Operation-to-Permission Mapping in `host/ai.rs`

```rust
fn permission_for_operation(op: &AiOperationType) -> &'static str {
    match op {
        AiOperationType::Chat => "use ai chat",
        AiOperationType::Embedding => "use ai embeddings",
        AiOperationType::ImageGeneration => "use ai image generation",
        // Future operations default to base permission only
        _ => "use ai",
    }
}
```

Insert permission check block after request deserialization (after line ~430 in current `host/ai.rs`), before `resolve_provider`:

```rust
// Permission check — before rate limit and budget
let user = &caller.data().request.user;
if !user.has_permission("use ai") {
    if user.id.is_none() {
        tracing::warn!(plugin = %plugin_name, "AI request denied: anonymous user (authentication required)");
    } else {
        tracing::warn!(plugin = %plugin_name, user_id = ?user.id, "AI request denied: user lacks 'use ai' permission");
    }
    return Ok(ERR_AI_PERMISSION_DENIED);
}
let op_perm = permission_for_operation(&ai_request.operation);
if op_perm != "use ai" && !user.has_permission(op_perm) {
    tracing::warn!(plugin = %plugin_name, user_id = ?user.id, permission = op_perm, "AI request denied: user lacks operation permission");
    return Ok(ERR_AI_PERMISSION_DENIED);
}
```

**CRITICAL**: Clone any values from `caller.data()` BEFORE any `.await` calls — the borrow cannot be held across await points. The permission check above is synchronous (Vec scan), so no await issue here.

### Anonymous User Detection

`UserContext` in `request_state.rs` has:
- `id: Uuid` — `Uuid::nil()` for anonymous
- `authenticated: bool` — `false` for anonymous
- `permissions: Vec<String>` — empty for anonymous

Check `!user.authenticated` to distinguish anonymous from authenticated-but-lacking-permission.

### Existing Patterns to Reuse

| Pattern | Location | Usage |
|---------|----------|-------|
| `require_admin` | `routes/helpers.rs:70` | Template for `require_permission` |
| `require_admin_json` | `routes/helpers.rs:89` | Template for `require_permission_json` |
| `AVAILABLE_PERMISSIONS` | `routes/admin_user.rs:24` | Extend with 6 AI perms |
| `user_has_permission` | `permissions.rs` | Called by new helper |
| `UserContext::has_permission` | `tap/request_state.rs` | Used in host/ai.rs |
| `ERR_AI_BUDGET_EXCEEDED = -26` | `plugin-sdk/host_errors.rs` | Pattern for -27 |
| `AI_BUDGET_LOCK` | `tests/integration_test.rs` | Pattern for test serialization |

### Testing Strategy

Use existing test infrastructure from `crates/kernel/tests/common/mod.rs`:
- `shared_app()` for `&'static TestApp`
- `create_and_login_admin` / `create_and_login_user` for authenticated users
- Add permissions to a test role via direct SQL (`INSERT INTO role_permissions`)
- Assign role to test user via direct SQL (`INSERT INTO user_roles`)
- Use `AI_BUDGET_LOCK` or a new `AI_PERMISSION_LOCK` mutex for tests that mutate role permissions

**IMPORTANT**: The host function permission check occurs inside WASM execution, which tests cannot directly exercise without a compiled WASM plugin. Test the permission check at the **service level** by:
1. Testing `require_permission` helper via HTTP requests to admin routes (provider/budget pages)
2. Testing the `UserContext::has_permission` path by verifying the permission data flows correctly through the role system

For host function testing, validate that the permission mapping function works and the check logic is correct via unit tests in `host/ai.rs`.

### Files to Create

None — all changes are modifications to existing files.

### Files to Modify

| File | Change |
|------|--------|
| `crates/plugin-sdk/src/host_errors.rs` | Add `ERR_AI_PERMISSION_DENIED = -27` + doc |
| `crates/kernel/src/routes/helpers.rs` | Add `require_permission` + `require_permission_json` |
| `crates/kernel/src/routes/admin_user.rs` | Extend `AVAILABLE_PERMISSIONS` with 6 AI perms |
| `crates/kernel/src/host/ai.rs` | Add permission check before rate limit/budget |
| `crates/kernel/src/routes/admin_ai_provider.rs` | Replace `require_admin` with `require_permission` |
| `crates/kernel/src/routes/admin_ai_budget.rs` | Replace `require_admin` with `require_permission` |
| `crates/kernel/tests/integration_test.rs` | Add permission integration tests |

### Project Structure Notes

- All modifications are to existing files — no new files or modules needed
- Permission strings use Trovato convention: lowercase words separated by spaces
- `AVAILABLE_PERMISSIONS` in `admin_user.rs` is the canonical list for admin UI — append AI perms at the end
- The `require_permission` helper lives alongside `require_admin` in `helpers.rs` — same module, same visibility

### Constraints and Pitfalls

1. **Do NOT create a separate AI permissions service** — use the existing `PermissionService` which already handles role-based permission lookup with caching
2. **Do NOT modify `UserContext` struct** — it already has `permissions: Vec<String>` and `has_permission()`. AI permissions are just new string values in the same system.
3. **Do NOT add a `trovato_ai` plugin in this story** — that's a future story. Permissions go directly into `AVAILABLE_PERMISSIONS` for now.
4. **`caller.data()` borrow rule**: In `host/ai.rs`, permission checks are synchronous (Vec scan) so no await issue, but clone needed values before any subsequent awaits.
5. **Admin users bypass permission checks** — `PermissionService::user_has_permission` returns `true` for admin users. The `require_permission` helper should also short-circuit on `user.is_admin`.
6. **Rate limit check comes AFTER permission check** — deny unpermitted users before consuming rate limit tokens.

### References

- [Source: docs/design/ai-integration.md#Section 1.4 — AI Permissions]
- [Source: docs/ritrovo/epic-03.md#Story 31.4 — AI Permissions]
- [Source: crates/kernel/src/routes/helpers.rs — require_admin pattern]
- [Source: crates/kernel/src/permissions.rs — PermissionService]
- [Source: crates/kernel/src/tap/request_state.rs — UserContext::has_permission]
- [Source: crates/plugin-sdk/src/host_errors.rs — AI error code range]
- [Source: crates/kernel/src/host/ai.rs — ai_request host function]

### Previous Story Intelligence

**From Story 31.2 (AI Host Function):**
- `func_wrap_async` with `Box::new(async move { ... })` for async host functions
- Clone all `caller.data()` fields before any `.await` — critical pattern
- Services accessed via `caller.data().request.services()` returning `Option<&RequestServices>`
- Rate limiter and message validation established the check-ordering pattern
- Tera `{% import %}` must be at template top level, not inside `{% block %}`

**From Story 31.3 (Token Budgets):**
- Budget check goes in `host/ai.rs` between rate limit and HTTP call — permission check inserts BEFORE rate limit
- Atomic JSONB operations for user data updates
- `AI_BUDGET_LOCK` mutex pattern for integration tests that mutate shared config
- 8 integration tests established; this story adds ~5 more

**From adversarial review (31.3):**
- Saturating `u32→i32` conversion for token counts
- `.max(0)` guard on SQL aggregate sums
- Composite index for multi-column queries

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

None — no debug issues encountered.

### Completion Notes List

- `UserContext.id` is `Uuid` (not `Option<Uuid>`). Anonymous users have `id = Uuid::nil()` and `authenticated = false`. Used `!user.authenticated` for anonymous detection instead of `user.id.is_none()`.
- `require_permission_json` returns `Result<(), (StatusCode, Json<JsonError>)>` (not `Result<User, ...>`) — the user object is not needed by JSON callers; this matches the simpler pattern.
- Integration tests create roles, add permissions, assign to users via direct SQL, and invalidate the `PermissionService` DashMap cache before testing HTTP endpoints.
- Added `AI_PERMISSION_LOCK: Mutex<()>` for test serialization since permission tests mutate shared role/permission state.
- Host function permission tests (7.1-7.3 from original plan) require a compiled WASM plugin; replaced with HTTP-level integration tests + unit test for `permission_for_operation` mapping.

### File List

**Modified:**
- `crates/plugin-sdk/src/host_errors.rs` — Added `ERR_AI_PERMISSION_DENIED = -27` constant + doc table entry
- `crates/kernel/src/routes/helpers.rs` — Added `require_permission()` and `require_permission_json()` helpers
- `crates/kernel/src/routes/admin_user.rs` — Extended `AVAILABLE_PERMISSIONS` with 6 AI permission strings
- `crates/kernel/src/host/ai.rs` — Added `permission_for_operation()` fn + permission check block in `ai_request()` + unit test
- `crates/kernel/src/routes/admin_ai_provider.rs` — Replaced `require_admin`/`require_admin_json` with `require_permission`/`require_permission_json` using `"configure ai"`
- `crates/kernel/src/routes/admin_ai_budget.rs` — Replaced `require_admin` with `require_permission` using `"view ai usage"` (dashboard) and `"configure ai"` (save/override)
- `crates/kernel/tests/integration_test.rs` — Added `AI_PERMISSION_LOCK` + 6 integration tests (review: fixed `RETURNING id` pattern, added write-permission differentiation test)
