# Story 32.1: Comment Service Layer

Status: done

## Story

As a plugin developer,
I want comment CRUD operations to go through a service layer with tap hook dispatch,
so that plugins can react to comment creation, updates, and deletions the same way they can for items.

## Background

Story 32.1 exists because the service-layer bypass audit (which routed `admin_content.rs`, `admin_taxonomy.rs`, `front.rs`, and `comment.rs` item calls through `ItemService` / `CategoryService`) revealed that **comments have no service layer at all**. There are 16 direct `Comment::*` model calls across two route files:

- `routes/comment.rs` — 9 calls (public API: list, create, read, update, delete)
- `routes/admin.rs` — 7 calls (admin moderation: list, read, update, delete)

Without a `CommentService`, plugins cannot:
- React to new comments (e.g., spam detection, notification, moderation queue)
- React to comment edits or deletions (e.g., audit logging, cache invalidation)
- Enforce access control via `tap_comment_access`

Additionally, the two `// TODO: Add proper permission check for admins` notes in `comment.rs` (lines 441, 568) should be resolved as part of this work.

## Acceptance Criteria

1. **AC1: CommentService struct** — A `CommentService` exists in `crates/kernel/src/services/comment.rs` with `Arc<TapDispatcher>` and `PgPool`, following the `ItemService` pattern. DashMap caching is optional (comments are less frequently re-read than items) but the dispatcher integration is required.

2. **AC2: Tap hooks fire** — `tap_comment_insert` fires after `Comment::create`, `tap_comment_update` fires after `Comment::update`, and `tap_comment_delete` fires before `Comment::delete`. Each tap receives the comment serialized as JSON plus a `RequestState` with the acting user's `UserContext`.

3. **AC3: Access control** — `CommentService::check_access()` follows the `ItemService` pattern: admin short-circuit, then `tap_comment_access` dispatch, then fall back to permission-based checks (`"edit own comments"`, `"edit any comment"`, `"delete own comments"`, `"delete any comment"`, `"post comments"`). This resolves the two TODO comments in `comment.rs`.

4. **AC4: Route migration — comment.rs** — All 9 `Comment::*` calls in `routes/comment.rs` are replaced with `state.comments().*` service calls. `Item::find_by_id` calls (already migrated to `state.items().load()`) remain as-is.

5. **AC5: Route migration — admin.rs** — All 7 `Comment::*` calls in `routes/admin.rs` are replaced with `state.comments().*` service calls. Admin handlers use `admin_user_context()` from `routes/helpers.rs` for the `UserContext`.

6. **AC6: AppState integration** — `CommentService` is wired into `AppState` as `Option<Arc<CommentService>>` (comment system is plugin-gated). Accessor: `pub fn comments(&self) -> &CommentService`. Initialized when the `"comments"` plugin is enabled.

7. **AC7: Tests pass** — All existing integration tests continue to pass. New unit tests cover `CommentService::check_access()` logic (admin bypass, author-owns-comment, permission fallback).

## Tasks / Subtasks

- [x] Task 1: Create `CommentService` (AC: #1, #2)
  - [x] 1.1 Create `crates/kernel/src/services/comment.rs` with struct, `new()`, and `PgPool` + `Arc<TapDispatcher>`
  - [x] 1.2 Implement `load(id) -> Result<Option<Comment>>`
  - [x] 1.3 Implement `create(input, &UserContext) -> Result<Comment>` with `tap_comment_insert`
  - [x] 1.4 Implement `update(id, input, &UserContext) -> Result<Option<Comment>>` with `tap_comment_update`
  - [x] 1.5 Implement `delete(id, &UserContext) -> Result<bool>` with `tap_comment_delete`
  - [x] 1.6 Implement `list_for_item(item_id) -> Result<Vec<Comment>>`
  - [x] 1.7 Implement `list_all(limit, offset) -> Result<Vec<Comment>>`
  - [x] 1.8 Implement `list_by_status(status, limit, offset) -> Result<Vec<Comment>>`
  - [x] 1.9 Implement `count_all() -> Result<i64>`
  - [x] 1.10 Add `pub mod comment;` to `services/mod.rs`

- [x] Task 2: Implement access control (AC: #3)
  - [x] 2.1 Implement `check_access(&Comment, operation, &UserContext) -> Result<bool>`
  - [x] 2.2 Admin short-circuit (`is_admin() -> true`)
  - [x] 2.3 Author-owns-comment check for edit/delete own
  - [x] 2.4 `tap_comment_access` dispatch with deny/grant/neutral aggregation
  - [x] 2.5 Permission fallback: `"post comments"`, `"edit own comments"`, `"edit any comment"`, `"delete own comments"`, `"delete any comment"`

- [x] Task 3: Wire into AppState (AC: #6)
  - [x] 3.1 Add `comments: Option<Arc<CommentService>>` to `AppStateInner`
  - [x] 3.2 Initialize conditionally when `"comments"` plugin is enabled
  - [x] 3.3 Add accessor `pub fn comments(&self) -> &CommentService`

- [x] Task 4: Migrate `routes/comment.rs` (AC: #4)
  - [x] 4.1 Replace 9 `Comment::*` calls with `state.comments().*`
  - [x] 4.2 Remove `Comment` from models import (keep `CreateComment`, `UpdateComment`)
  - [x] 4.3 Remove TODO comments, use `check_access` for admin permission checks

- [x] Task 5: Migrate `routes/admin.rs` (AC: #5)
  - [x] 5.1 Replace 7 `Comment::*` calls with `state.comments().*`
  - [x] 5.2 Build `UserContext` via `admin_user_context()` for mutation operations
  - [x] 5.3 Remove `Comment` from models import if no longer needed as a type

- [x] Task 6: Tests (AC: #7)
  - [x] 6.1 Unit tests for `check_access` (admin bypass, author owns, permission fallback)
  - [x] 6.2 Verify all existing integration tests pass
  - [x] 6.3 `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all`

## Dev Notes

### Pattern to Follow

`CommentService` should mirror `ItemService` in `crates/kernel/src/content/item_service.rs`. Key patterns:

- Inner `Arc` wrapping for cheap cloning into route handlers
- `TapDispatcher::dispatch(tap_name, &json, state)` for hook invocation
- `RequestState::without_services(user.clone())` for the tap execution context
- `check_access` with admin short-circuit at line 243 of `item_service.rs`

### Caching Decision

Unlike `ItemService` which caches individual items in a `DashMap`, `CommentService` may not benefit from per-comment caching since comments are typically loaded as lists rather than individually. Consider:

- **Skip per-comment cache** for simplicity (comments are loaded in bulk for display)
- **Optional**: Cache `list_for_item` results keyed by `item_id`, invalidated on create/update/delete for that item

### Plugin Gate Interaction

Comments are gated via `plugin_gate!(gate_comments, "comments")` in `routes/mod.rs`. The `CommentService` in `AppState` should use `Option<Arc<CommentService>>` and only be initialized when the plugin is enabled, matching the kernel minimality rules for plugin-optional services.

### Existing Bypasses Inventory

**`routes/comment.rs`** (public API):
| Call | Method |
|------|--------|
| `Comment::list_for_item(state.db(), item_id)` | `list_for_item` |
| `Comment::find_by_id(state.db(), parent_id)` | `load` (parent validation) |
| `Comment::create(state.db(), input)` | `create` |
| `Comment::find_by_id(state.db(), id)` | `load` (get_comment) |
| `Comment::find_by_id(state.db(), id)` | `load` (update pre-check) |
| `Comment::update(state.db(), id, input)` | `update` |
| `Comment::find_by_id(state.db(), id)` | `load` (delete pre-check) |
| `Comment::delete(state.db(), id)` | `delete` |

**`routes/admin.rs`** (admin moderation):
| Call | Method |
|------|--------|
| `Comment::list_by_status(...)` | `list_by_status` |
| `Comment::list_all(...)` | `list_all` |
| `Comment::count_all(...)` | `count_all` |
| `Comment::find_by_id(state.db(), id)` | `load` |
| `Comment::update(state.db(), id, ...)` | `update` (approve) |
| `Comment::update(state.db(), id, ...)` | `update` (unpublish) |
| `Comment::delete(state.db(), id)` | `delete` |

### References

- [Source: crates/kernel/src/content/item_service.rs — ItemService pattern]
- [Source: crates/kernel/src/gather/category_service.rs — CategoryService pattern]
- [Source: crates/kernel/src/routes/comment.rs — public comment API routes]
- [Source: crates/kernel/src/routes/admin.rs — admin comment moderation routes]
- [Source: crates/kernel/src/routes/helpers.rs — admin_user_context helper]
- [Source: docs/kernel-minimality-audit.md — kernel vs plugin placement rules]
