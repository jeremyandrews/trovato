# Story 32.2: User Service Layer

Status: done

## Story

As a plugin developer,
I want user CRUD operations and authentication events to go through a service layer with tap hook dispatch,
so that plugins can reliably react to user registration, login, profile changes, and deletion without depending on fragile route-level tap invocations.

## Background

The service-layer audit revealed that **User has no service layer despite being the most heavily used model in the codebase**. There are approximately 40 direct `User::*` model calls across 14 route files, plus 7 tap dispatch callsites scattered across `auth.rs` and `admin_user.rs`.

Without a `UserService`:
- Tap hooks (`tap_user_login`, `tap_user_logout`, `tap_user_register`, `tap_user_update`, `tap_user_delete`) are invoked ad-hoc from route handlers — any new route that creates/modifies users must remember to dispatch taps manually
- There is no centralized cache for user lookups despite `User::find_by_id` being called ~20 times across routes (often for the same user in the same request cycle — e.g., author info in list views)
- No `check_access` pattern for user operations (profile edit, password change)
- The `require_login`, `require_admin`, and `require_permission` helpers in `helpers.rs` all call `User::find_by_id` independently — a cached service would deduplicate these lookups

### Scope Boundaries

This story covers **User model operations only**. Related models that interact with User but have their own lifecycle:
- `Role` / role assignments → Story 32.3 (RoleService)
- `EmailVerificationToken` → remains direct-call (short-lived token, no plugin interaction needed)
- `PasswordResetToken` → remains direct-call (same rationale)
- `ApiToken` → remains direct-call (MCP auth, low call count)

## Acceptance Criteria

1. **AC1: UserService struct** — A `UserService` exists in `crates/kernel/src/services/user.rs` with `PgPool`, `Arc<TapDispatcher>`, and `DashMap<Uuid, User>` cache, following the `ItemService` pattern (inner Arc wrapping for cheap cloning).

2. **AC2: Tap hooks fire from service** — All 5 user tap hooks fire from `UserService` methods, not from route handlers:
   - `tap_user_register` fires after `UserService::create` / `UserService::create_with_status`
   - `tap_user_update` fires after `UserService::update` / `UserService::update_password`
   - `tap_user_delete` fires before `UserService::delete`
   - `tap_user_login` fires from `UserService::record_login`
   - `tap_user_logout` fires from `UserService::record_logout`
   Each tap receives `{ "user_id": "<uuid>" }` plus a `RequestState` with the acting user's `UserContext`.

3. **AC3: DashMap cache** — `UserService` caches users by UUID. `find_by_id` checks cache first. Cache is invalidated on `create`, `update`, `update_password`, and `delete`. `find_by_name` and `find_by_mail` are not cached (used only for uniqueness checks).

4. **AC4: Route migration — auth.rs** — All `User::*` calls in `routes/auth.rs` are replaced with `state.users().*` service calls. All 4 tap dispatch blocks in `auth.rs` are removed (service handles dispatch).

5. **AC5: Route migration — admin_user.rs** — All `User::*` calls in `routes/admin_user.rs` are replaced with `state.users().*` service calls. All 3 tap dispatch blocks in `admin_user.rs` are removed.

6. **AC6: Route migration — remaining files** — `User::find_by_id` calls in `helpers.rs`, `admin.rs`, `admin_content.rs`, `admin_ai_budget.rs`, `comment.rs`, `item.rs`, `api_chat.rs`, `lock.rs`, `password_reset.rs`, and `install.rs` are replaced with `state.users().find_by_id()`.

7. **AC7: Route migration — MCP server** — `User::find_by_id` call in `crates/mcp-server/src/auth.rs` is replaced with `state.users().find_by_id()`.

8. **AC8: AppState integration** — `UserService` is wired into `AppState` as `Arc<UserService>` (always present, not optional). Accessor: `pub fn users(&self) -> &UserService`.

9. **AC9: Tests pass** — All existing integration tests continue to pass. New unit tests cover cache hit/miss behavior and tap dispatch verification.

## Tasks / Subtasks

- [x] Task 1: Create `UserService` (AC: #1, #2, #3)
  - [x] 1.1 Create `crates/kernel/src/services/user.rs` with struct, `new()`, `PgPool` + `Arc<TapDispatcher>` + `DashMap<Uuid, User>`
  - [x] 1.2 Implement `find_by_id(id) -> Result<Option<User>>` with DashMap cache
  - [x] 1.3 Implement `find_by_name(name) -> Result<Option<User>>` (no cache, uniqueness checks only)
  - [x] 1.4 Implement `find_by_mail(mail) -> Result<Option<User>>` (no cache)
  - [x] 1.5 Implement `create(input, &UserContext) -> Result<User>` with `tap_user_register` + cache insert
  - [x] 1.6 Implement `create_with_status(input, status, &UserContext) -> Result<User>` with `tap_user_register` + cache insert
  - [x] 1.7 Implement `update(id, input, &UserContext) -> Result<Option<User>>` with `tap_user_update` + cache re-population
  - [x] 1.8 Implement `update_password(id, password, &UserContext) -> Result<bool>` with `tap_user_update` + cache invalidation
  - [x] 1.9 Implement `delete(id, &UserContext) -> Result<bool>` with `tap_user_delete` (before) + cache invalidation
  - [x] 1.10 Implement `record_login(user) -> Result<()>` — calls `User::touch_login` + dispatches `tap_user_login`
  - [x] 1.11 Implement `record_logout(user_id) -> Result<()>` — dispatches `tap_user_logout`
  - [x] 1.12 Implement `list() -> Result<Vec<User>>`, `list_paginated(limit, offset) -> Result<Vec<User>>`, `count() -> Result<i64>`
  - [x] 1.13 Implement `touch_access(id) -> Result<()>` (no tap, no cache invalidation — frequent, low-value)
  - [x] 1.14 Implement `verify_password(&User, password) -> bool` (delegate to model instance method)
  - [x] 1.15 Add `pub mod user;` to `services/mod.rs`

- [x] Task 2: Wire into AppState (AC: #8)
  - [x] 2.1 Add `users: Arc<UserService>` to `AppStateInner`
  - [x] 2.2 Initialize in `AppState::new()` (always present)
  - [x] 2.3 Add accessor `pub fn users(&self) -> &UserService`

- [x] Task 3: Migrate `routes/auth.rs` (AC: #4)
  - [x] 3.1 Replace ~15 `User::*` calls with `state.users().*`
  - [x] 3.2 Remove 4 tap dispatch blocks (`tap_user_login`, `tap_user_logout`, `tap_user_register` x1, `tap_user_update` x2)
  - [x] 3.3 Replace `User::create_with_status` with `state.users().create_with_status()`
  - [x] 3.4 Replace `User::touch_login` with `state.users().record_login()`

- [x] Task 4: Migrate `routes/admin_user.rs` (AC: #5)
  - [x] 4.1 Replace ~13 `User::*` calls with `state.users().*`
  - [x] 4.2 Remove 3 tap dispatch blocks (`tap_user_register` x1, `tap_user_update` x1, `tap_user_delete` x1)

- [x] Task 5: Migrate remaining route files (AC: #6)
  - [x] 5.1 `helpers.rs` — 4 `User::find_by_id` calls → `state.users().find_by_id()`
  - [x] 5.2 `admin.rs` — 4 `User::find_by_id` calls (file owner, comment author lookups)
  - [x] 5.3 `admin_content.rs` — 1 `User::find_by_id` call (author cache in list view)
  - [x] 5.4 `admin_ai_budget.rs` — 2 `User::find_by_id` calls
  - [x] 5.5 `comment.rs` — 4 `User::find_by_id` calls (author info)
  - [x] 5.6 `item.rs` — 2 `User::find_by_id` calls (API response author info)
  - [x] 5.7 `api_chat.rs` — 1 `User::find_by_id` call
  - [x] 5.8 `lock.rs` — 1 `User::find_by_id` call
  - [x] 5.9 `password_reset.rs` — 1 `User::find_by_mail` + 1 `User::update_password` call
  - [x] 5.10 `install.rs` — 2 `User::find_by_*` + 1 `User::create` call

- [x] Task 6: Migrate MCP server (AC: #7)
  - [x] 6.1 `crates/mcp-server/src/auth.rs` — 1 `User::find_by_id` call

- [x] Task 7: Tests (AC: #9)
  - [x] 7.1 Unit tests for DashMap cache (hit after find_by_id, invalidation on update/delete)
  - [x] 7.2 Verify all existing integration tests pass
  - [x] 7.3 `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all`

## Dev Notes

### Pattern to Follow

`UserService` should mirror `ItemService` in `crates/kernel/src/content/item_service.rs`. Key patterns:

- Inner `Arc` wrapping for cheap cloning: `struct UserService { inner: Arc<UserServiceInner> }`
- `TapDispatcher::dispatch(tap_name, &json, state)` for hook invocation
- `RequestState::without_services(user.clone())` for the tap execution context
- DashMap cache with `invalidate(id)` on mutations

### Tap Dispatch Migration

Currently, tap hooks are dispatched from routes with this pattern:

```rust
// In auth.rs / admin_user.rs
let tap_input = serde_json::json!({ "user_id": user.id.to_string() });
let tap_state = RequestState::without_services(UserContext::authenticated(user.id, vec![]));
state.tap_dispatcher().dispatch("tap_user_register", &tap_input.to_string(), tap_state).await;
```

This moves into `UserService::create()`:

```rust
pub async fn create(&self, input: CreateUser, acting_user: &UserContext) -> Result<User> {
    let user = User::create(&self.inner.pool, input).await?;
    self.inner.cache.insert(user.id, user.clone());
    let json = serde_json::json!({ "user_id": user.id.to_string() });
    let state = RequestState::without_services(acting_user.clone());
    let _ = self.inner.dispatcher.dispatch("tap_user_register", &json.to_string(), state).await;
    Ok(user)
}
```

### Login/Logout Special Cases

`record_login` and `record_logout` are not standard CRUD — they combine a model operation (or no operation for logout) with a tap dispatch. The `&UserContext` passed to taps should represent the user who is logging in/out (not an admin acting on their behalf).

### Cache Sizing

Unlike `ItemService` which caches items that might number in the thousands, `UserService` caches users which are typically fewer. The DashMap is unbounded; a `MAX_CACHE_ENTRIES` cap similar to `RedirectCache` is recommended but optional for this story.

### helpers.rs Impact

The `require_login`, `require_admin`, and `require_permission` helpers all call `User::find_by_id`. After migration, these will hit the DashMap cache, which means repeated admin checks in a single request won't issue redundant DB queries.

### install.rs Special Case

The installer creates the initial admin user before `AppState` is fully initialized. Verify that `UserService` is available during the install flow, or keep the direct `User::create` call in `install.rs` as an exception (document why).

### Existing Bypasses Inventory

**`routes/auth.rs`** (15 calls + 4 tap dispatches):
| Call | Method | Type |
|------|--------|------|
| `User::find_by_name(state.db(), &request.username)` | `find_by_name` | read |
| `User::touch_login(state.db(), user.id)` | `record_login` | mutation |
| `dispatch("tap_user_login", ...)` | — | tap |
| `dispatch("tap_user_logout", ...)` | — | tap |
| `User::find_by_name(state.db(), username)` | `find_by_name` | read |
| `User::find_by_mail(state.db(), mail)` | `find_by_mail` | read |
| `User::create_with_status(state.db(), input, 0)` | `create_with_status` | mutation |
| `dispatch("tap_user_register", ...)` | — | tap |
| `User::update(state.db(), verification.user_id, ...)` | `update` | mutation |
| `User::find_by_id(state.db(), verification.user_id)` | `find_by_id` | read |
| `User::find_by_mail(state.db(), &new_email)` | `find_by_mail` | read |
| `User::update(state.db(), user.id, update)` | `update` | mutation |
| `User::find_by_id(state.db(), user_id)` | `find_by_id` | read |
| `User::find_by_name(state.db(), name)` | `find_by_name` | read |
| `User::find_by_mail(state.db(), mail)` | `find_by_mail` | read |
| `User::update(state.db(), user.id, update)` | `update` | mutation |
| `dispatch("tap_user_update", ...)` x2 | — | tap |
| `User::update_password(state.db(), user.id, ...)` | `update_password` | mutation |

**`routes/admin_user.rs`** (13 calls + 3 tap dispatches):
| Call | Method | Type |
|------|--------|------|
| `User::list(state.db())` | `list` | read |
| `User::find_by_name(state.db(), &form.name)` | `find_by_name` | read |
| `User::find_by_mail(state.db(), &form.mail)` | `find_by_mail` | read |
| `User::create(state.db(), input)` | `create` | mutation |
| `dispatch("tap_user_register", ...)` | — | tap |
| `User::find_by_id(state.db(), user_id)` x2 | `find_by_id` | read |
| `User::find_by_name(state.db(), &form.name)` | `find_by_name` | read |
| `User::find_by_mail(state.db(), &form.mail)` | `find_by_mail` | read |
| `User::update(state.db(), user_id, input)` | `update` | mutation |
| `User::update_password(state.db(), user_id, password)` | `update_password` | mutation |
| `dispatch("tap_user_update", ...)` | — | tap |
| `User::delete(state.db(), user_id)` | `delete` | mutation |
| `dispatch("tap_user_delete", ...)` | — | tap |

**Other files** (18 `User::find_by_id` + 2 misc):
| File | Calls | Notes |
|------|-------|-------|
| `helpers.rs` | 4x `find_by_id` | `require_login`, `require_admin`, `require_permission`, `inject_site_context` |
| `admin.rs` | 4x `find_by_id` | file owner, comment author lookups |
| `comment.rs` | 4x `find_by_id` | comment author info |
| `item.rs` | 2x `find_by_id` | API response author info |
| `admin_content.rs` | 1x `find_by_id` | list view author cache |
| `admin_ai_budget.rs` | 2x `find_by_id` | budget admin |
| `api_chat.rs` | 1x `find_by_id` | chat stream |
| `lock.rs` | 1x `find_by_id` | content lock display |
| `password_reset.rs` | 1x `find_by_mail` + 1x `update_password` | password reset flow |
| `install.rs` | 2x `find_by_*` + 1x `create` | installer (may be exception) |
| `mcp-server/auth.rs` | 1x `find_by_id` | MCP token resolution |

### References

- [Source: crates/kernel/src/content/item_service.rs — ItemService pattern]
- [Source: crates/kernel/src/models/user.rs — User model]
- [Source: crates/kernel/src/routes/auth.rs — auth routes with tap dispatches]
- [Source: crates/kernel/src/routes/admin_user.rs — admin user routes with tap dispatches]
- [Source: crates/kernel/src/routes/helpers.rs — require_login/require_admin helpers]
