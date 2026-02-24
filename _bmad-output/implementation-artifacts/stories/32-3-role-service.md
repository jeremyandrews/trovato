# Story 32.3: Role Service Layer

Status: ready-for-dev

## Story

As a site administrator,
I want role and permission mutations to go through a service layer that invalidates the permission cache,
so that permission changes take effect immediately rather than serving stale cached permissions.

## Background

Story 32.3 addresses two problems discovered in the service-layer audit:

1. **No RoleService** — There are 18 direct `Role::*` model calls in `routes/admin_user.rs` plus 3 in `permissions.rs` and 1 in `services/ai_token_budget.rs`. All admin role CRUD (create, update, delete, permission assignment) bypasses any service layer.

2. **Permission cache is never invalidated in production** — `PermissionService` maintains a `DashMap<Uuid, CachedPermissions>` cache, and defines `invalidate_user()` and `invalidate_all()` methods, but **neither is called from any production route handler**. When an admin changes a role's permissions or assigns/removes roles from users via the UI, the cached permissions remain stale until natural expiration or server restart. This is a correctness bug.

A `RoleService` that wraps role mutations and calls `PermissionService::invalidate_all()` on permission changes (and `invalidate_user()` on role assignment changes) would fix this systematically.

### Scope Boundaries

- Role CRUD and permission management only
- User-role assignment/removal
- Permission cache invalidation on mutations
- Does NOT cover user CRUD (Story 32.2) or tap hooks for role changes (future work)

## Acceptance Criteria

1. **AC1: RoleService struct** — A `RoleService` exists in `crates/kernel/src/services/role.rs` with `PgPool` and a reference to `PermissionService` for cache invalidation. No DashMap cache (roles are infrequently looked up individually). No `TapDispatcher` (no role-specific tap hooks exist yet).

2. **AC2: Permission cache invalidation** — `RoleService` calls `PermissionService::invalidate_all()` after any of: `add_permission`, `remove_permission`, `save_permissions` (bulk update), `delete` (role deletion cascades to `role_permissions`). Calls `PermissionService::invalidate_user(user_id)` after `assign_to_user` or `remove_from_user`.

3. **AC3: Well-known role protection** — `RoleService::delete()` rejects deletion of `ANONYMOUS_ROLE_ID` and `AUTHENTICATED_ROLE_ID`, matching the existing model-level check but returning a clear error rather than a silent `false`.

4. **AC4: Route migration — admin_user.rs** — All 18 `Role::*` calls in `routes/admin_user.rs` are replaced with `state.roles().*` service calls.

5. **AC5: Service-layer callers migrated** — `Role::get_user_roles` in `ai_token_budget.rs` and `Role::get_permissions` / `Role::get_user_permissions` in `permissions.rs` are replaced with `RoleService` calls (or remain as model calls with a documented justification, since `PermissionService` is initialized before `RoleService`).

6. **AC6: AppState integration** — `RoleService` is wired into `AppState` as `Arc<RoleService>` (always present). Accessor: `pub fn roles(&self) -> &RoleService`.

7. **AC7: Tests pass** — All existing integration tests continue to pass. New unit tests verify that permission cache is invalidated after role permission changes.

## Tasks / Subtasks

- [ ] Task 1: Create `RoleService` (AC: #1, #2, #3)
  - [ ] 1.1 Create `crates/kernel/src/services/role.rs` with struct, `new()`, `PgPool` + `Arc<PermissionService>`
  - [ ] 1.2 Implement `find_by_id(id) -> Result<Option<Role>>`
  - [ ] 1.3 Implement `find_by_name(name) -> Result<Option<Role>>`
  - [ ] 1.4 Implement `list() -> Result<Vec<Role>>`
  - [ ] 1.5 Implement `create(name) -> Result<Role>`
  - [ ] 1.6 Implement `update(id, name) -> Result<Option<Role>>`
  - [ ] 1.7 Implement `delete(id) -> Result<bool>` with well-known role protection + `invalidate_all()`
  - [ ] 1.8 Implement `get_permissions(role_id) -> Result<Vec<String>>`
  - [ ] 1.9 Implement `add_permission(role_id, permission) -> Result<()>` + `invalidate_all()`
  - [ ] 1.10 Implement `remove_permission(role_id, permission) -> Result<()>` + `invalidate_all()`
  - [ ] 1.11 Implement `save_permissions(role_id, permissions: &[String]) -> Result<()>` — bulk diff + `invalidate_all()` (replaces the add/remove loop in `save_permissions` handler)
  - [ ] 1.12 Implement `get_user_roles(user_id) -> Result<Vec<Role>>`
  - [ ] 1.13 Implement `assign_to_user(user_id, role_id) -> Result<()>` + `invalidate_user(user_id)`
  - [ ] 1.14 Implement `remove_from_user(user_id, role_id) -> Result<()>` + `invalidate_user(user_id)`
  - [ ] 1.15 Implement `get_user_permissions(user_id) -> Result<Vec<String>>`
  - [ ] 1.16 Add `pub mod role;` to `services/mod.rs`

- [ ] Task 2: Wire into AppState (AC: #6)
  - [ ] 2.1 Add `roles: Arc<RoleService>` to `AppStateInner`
  - [ ] 2.2 Initialize in `AppState::new()` after `PermissionService` (dependency order)
  - [ ] 2.3 Add accessor `pub fn roles(&self) -> &RoleService`

- [ ] Task 3: Migrate `routes/admin_user.rs` (AC: #4)
  - [ ] 3.1 Replace `Role::list(state.db())` calls with `state.roles().list()`
  - [ ] 3.2 Replace `Role::find_by_id/find_by_name` calls with `state.roles().find_by_id/find_by_name()`
  - [ ] 3.3 Replace `Role::create/update/delete` calls with `state.roles().create/update/delete()`
  - [ ] 3.4 Replace `Role::get_permissions` calls with `state.roles().get_permissions()`
  - [ ] 3.5 Replace `Role::add_permission/remove_permission` loop in `save_permissions` handler with `state.roles().save_permissions()` (bulk operation)
  - [ ] 3.6 Remove `Role` from models import (keep any input types if needed)

- [ ] Task 4: Evaluate service-layer callers (AC: #5)
  - [ ] 4.1 `permissions.rs` — `Role::get_permissions` and `Role::get_user_permissions` calls. These are in `PermissionService::load_user_permissions()` which runs before `RoleService` exists during init. Decision: keep as model calls with documented justification, OR restructure initialization order.
  - [ ] 4.2 `ai_token_budget.rs` — `Role::get_user_roles` call. Replace with `state.roles().get_user_roles()` if `AiTokenBudgetService` has access to `RoleService`.

- [ ] Task 5: Tests (AC: #7)
  - [ ] 5.1 Unit test: `add_permission` calls `invalidate_all()`
  - [ ] 5.2 Unit test: `remove_permission` calls `invalidate_all()`
  - [ ] 5.3 Unit test: `delete` role calls `invalidate_all()`
  - [ ] 5.4 Unit test: `assign_to_user` calls `invalidate_user(user_id)`
  - [ ] 5.5 Unit test: `delete` rejects well-known role IDs
  - [ ] 5.6 Integration test: permission change via admin UI takes effect immediately (no stale cache)
  - [ ] 5.7 Verify all existing integration tests pass
  - [ ] 5.8 `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all`

## Dev Notes

### The Cache Invalidation Bug

This is the primary motivation for `RoleService`. Today's flow:

1. Admin visits `/admin/people/permissions` and changes a role's permissions
2. `save_permissions` handler calls `Role::add_permission` / `Role::remove_permission` in a loop
3. `PermissionService` DashMap cache still holds old permissions
4. Next `require_permission()` check returns stale data
5. User must wait for cache expiry or server restart

After `RoleService`:

1. Admin changes permissions
2. `RoleService::save_permissions()` calls model methods then `PermissionService::invalidate_all()`
3. Next `require_permission()` check reloads from DB

### Why `invalidate_all()` for Permission Changes

When a role's permissions change, it affects **every user with that role**. Iterating all affected users to call `invalidate_user()` individually would require an extra DB query (`SELECT user_id FROM user_roles WHERE role_id = ?`). Since permission changes are infrequent admin operations, `invalidate_all()` is simpler and correct.

### Why `invalidate_user()` for Role Assignment

When a single user gains or loses a role, only that user's cached permissions are affected. `invalidate_user(user_id)` is more targeted than `invalidate_all()`.

### No Tap Hooks (Yet)

Unlike `UserService` (Story 32.2) which has 5 existing tap hooks, roles currently have no tap hooks. Adding `tap_role_create`, `tap_role_update`, `tap_role_delete`, `tap_role_permission_change` could be future work if plugins need to react to role changes. The `RoleService` architecture supports adding a `TapDispatcher` later without breaking changes.

### No DashMap Cache

Roles are looked up infrequently (admin UI only) and there are typically few roles (5-20). Caching would add complexity for negligible performance gain. If profiling reveals otherwise, a cache can be added later.

### PermissionService Initialization Order

`PermissionService::load_user_permissions()` calls `Role::get_permissions()` and `Role::get_user_permissions()` directly. Since `PermissionService` is initialized before `RoleService` in `AppState::new()`, these calls cannot go through `RoleService`. This is acceptable because:
- These are read-only operations (no cache invalidation needed)
- `PermissionService` holds its own `PgPool` reference
- The circular dependency would be architecturally worse than the direct calls

Document this exception with a comment in `permissions.rs`.

### `save_permissions` Bulk Operation

The current `save_permissions` handler in `admin_user.rs` loops over all roles, diffs current vs. submitted permissions, and calls `add_permission` / `remove_permission` individually. `RoleService::save_permissions(role_id, new_permissions)` should accept the desired permission set, compute the diff internally, apply changes, and call `invalidate_all()` once (not per-change).

### Existing Bypasses Inventory

**`routes/admin_user.rs`** (18 calls):
| Call | Method | Handler |
|------|--------|---------|
| `Role::list(state.db())` | `list` | `list_roles` |
| `Role::find_by_name(state.db(), &form.name)` | `find_by_name` | `add_role_submit` |
| `Role::create(state.db(), &form.name)` | `create` | `add_role_submit` |
| `Role::find_by_id(state.db(), role_id)` | `find_by_id` | `edit_role_form` |
| `Role::get_permissions(state.db(), role_id)` | `get_permissions` | `edit_role_form` |
| `Role::find_by_id(state.db(), role_id)` | `find_by_id` | `edit_role_submit` |
| `Role::find_by_name(state.db(), &form.name)` | `find_by_name` | `edit_role_submit` |
| `Role::get_permissions(state.db(), role_id)` | `get_permissions` | `edit_role_submit` |
| `Role::update(state.db(), role_id, &form.name)` | `update` | `edit_role_submit` |
| `Role::delete(state.db(), role_id)` | `delete` | `delete_role` |
| `Role::list(state.db())` | `list` | `permissions_matrix` |
| `Role::get_permissions(state.db(), role.id)` | `get_permissions` | `permissions_matrix` (loop) |
| `Role::list(state.db())` | `list` | `save_permissions` |
| `Role::get_permissions(state.db(), role.id)` | `get_permissions` | `save_permissions` (loop) |
| `Role::add_permission(state.db(), role.id, permission)` | `add_permission` | `save_permissions` (loop) |
| `Role::remove_permission(state.db(), role.id, permission)` | `remove_permission` | `save_permissions` (loop) |

**`routes/admin_ai_budget.rs`** (1 call):
| Call | Method | Handler |
|------|--------|---------|
| `Role::list(state.db())` | `list` | `show_budget_list` |

**`services/ai_token_budget.rs`** (1 call):
| Call | Method |
|------|--------|
| `Role::get_user_roles(pool, user_id)` | `get_user_roles` |

**`permissions.rs`** (3 calls — keep as model calls, see Dev Notes):
| Call | Method |
|------|--------|
| `Role::get_permissions(pool, well_known::ANONYMOUS_ROLE_ID)` | `get_permissions` |
| `Role::get_user_permissions(pool, user.id)` | `get_user_permissions` |
| `Role::get_permissions(pool, well_known::AUTHENTICATED_ROLE_ID)` | `get_permissions` |

### References

- [Source: crates/kernel/src/models/role.rs — Role model with well_known constants]
- [Source: crates/kernel/src/permissions.rs — PermissionService with invalidate methods]
- [Source: crates/kernel/src/routes/admin_user.rs — admin role CRUD and permission matrix]
- [Source: crates/kernel/src/services/ai_token_budget.rs — Role::get_user_roles usage]
- [Source: crates/kernel/src/content/item_service.rs — ItemService reference pattern]
