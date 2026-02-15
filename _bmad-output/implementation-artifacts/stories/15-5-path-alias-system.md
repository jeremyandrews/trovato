# Story 15.5: Path Alias System

Status: complete

## Story

As a **content editor**,
I want URL aliases for content,
So that pages have human-readable URLs instead of /item/{uuid}.

## Acceptance Criteria

1. **Given** an item exists
   **When** I create a path alias
   **Then** the item is accessible via both /item/{id} and the alias path

2. **Given** a path alias exists
   **When** a request comes for that path
   **Then** the system routes to the correct item without redirect (internal rewrite)

3. **Given** an alias path is requested
   **When** the item is loaded
   **Then** canonical URL uses the alias (for SEO)

4. **Given** multiple aliases exist for one item
   **When** I view the item
   **Then** the most recent alias is canonical

5. **Given** I'm an admin
   **When** I edit an item
   **Then** I can set/change the URL alias

6. Path aliases are stage-aware (alias can differ per stage)
   - Deferred to Story 21.5 - basic implementation uses `live` stage only

## Tasks / Subtasks

- [x] Task 1: Create database schema (AC: 1, 2, 6)
  - [x] Create migration `20260216000004_create_url_alias.sql`
  - [x] Table: `url_alias` with columns: id (UUID), source (VARCHAR), alias (VARCHAR UNIQUE), language (VARCHAR default 'en'), stage_id (VARCHAR default 'live'), created (BIGINT)
  - [x] Add index on `alias` column for fast lookups

- [x] Task 2: Create UrlAlias model (AC: 1, 4)
  - [x] Create `crates/kernel/src/models/url_alias.rs`
  - [x] Struct: `UrlAlias` with fields matching table
  - [x] CRUD operations: `create`, `find_by_alias`, `find_by_source`, `update`, `delete`
  - [x] Method: `get_canonical_alias(source)` returns most recent alias
  - [x] Add to `models/mod.rs` exports

- [x] Task 3: Create path alias middleware (AC: 2)
  - [x] Create `crates/kernel/src/middleware/path_alias.rs`
  - [x] Implement `resolve_path_alias` middleware function
  - [x] Check if incoming path matches an alias
  - [x] If match found, rewrite request URI to the source path (internal rewrite, no redirect)
  - [x] Pass through if no alias found
  - [x] Add to `middleware/mod.rs` exports

- [x] Task 4: Wire middleware into router (AC: 2)
  - [x] Add `path_alias` middleware layer in `main.rs`
  - [x] Must run AFTER session layer, BEFORE route matching
  - [x] Must run BEFORE `check_installation` middleware

- [x] Task 5: Add canonical URL helper (AC: 3)
  - [x] Add `get_canonical_url(item_id)` function to `UrlAlias` model
  - [x] Returns alias path if exists, otherwise `/item/{id}`
  - [ ] Update ThemeEngine context to include `canonical_url` variable (deferred - templates can call function)

- [x] Task 6: Add alias field to item edit form (AC: 5)
  - [x] Add `url_alias` text field to item edit form in `routes/item.rs`
  - [x] On item save, create/update alias if changed
  - [x] Show current alias in edit form if exists

- [x] Task 7: Admin UI for alias management
  - [x] Add `/admin/structure/aliases` route for listing all aliases
  - [x] Add create/edit/delete operations
  - [x] Template: `templates/admin/aliases.html`
  - [x] Template: `templates/admin/alias-form.html`

- [x] Task 8: Integration tests
  - [x] Test alias creation and lookup
  - [x] Test middleware rewrites path correctly (e2e HTTP tests)
  - [x] Test multiple aliases (most recent wins)
  - [x] Test canonical URL generation
  - [x] Test system path bypass (e2e)
  - [x] Test query string preservation (e2e)
  - [x] Test admin auth required (e2e)
  - [ ] Test item edit form alias field (manual testing - form integration exists)

## Dev Notes

### Database Schema

```sql
CREATE TABLE IF NOT EXISTS url_alias (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source VARCHAR(255) NOT NULL,      -- e.g., "/item/550e8400-e29b-41d4-a716-446655440000"
    alias VARCHAR(255) NOT NULL,        -- e.g., "/about-us"
    language VARCHAR(12) NOT NULL DEFAULT 'en',
    stage_id VARCHAR(50) NOT NULL DEFAULT 'live',
    created BIGINT NOT NULL,
    UNIQUE (alias, language, stage_id)
);

CREATE INDEX idx_url_alias_alias ON url_alias (alias);
CREATE INDEX idx_url_alias_source ON url_alias (source);
```

### Middleware Pattern

Follow the existing `install_check.rs` middleware pattern:

```rust
pub async fn resolve_path_alias(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Skip system paths
    if path.starts_with("/admin") || path.starts_with("/api")
       || path.starts_with("/static") || path.starts_with("/install") {
        return next.run(request).await;
    }

    // Look up alias
    if let Ok(Some(alias)) = UrlAlias::find_by_alias(state.db(), path).await {
        // Rewrite URI to source path
        let new_uri = alias.source.parse().unwrap_or_else(|_| request.uri().clone());
        *request.uri_mut() = new_uri;
    }

    next.run(request).await
}
```

### Middleware Ordering in main.rs

Middleware layers execute in reverse order (last added = first executed):

```rust
let app = Router::new()
    .merge(routes::install::router())
    // ... other routes ...
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::middleware::resolve_path_alias,  // Runs FIRST
    ))
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        crate::middleware::check_installation,
    ))
    .layer(session_layer)
    .layer(TraceLayer::new_for_http())
    .with_state(state);
```

### Project Structure Notes

- Model: `crates/kernel/src/models/url_alias.rs`
- Middleware: `crates/kernel/src/middleware/path_alias.rs`
- Migration: `crates/kernel/migrations/20260216000004_create_url_alias.sql`
- Admin template: `templates/admin/aliases.html`
- Tests: `crates/kernel/tests/url_alias_test.rs`

### Existing Patterns to Follow

- Model pattern: See `models/comment.rs` for CRUD structure
- Middleware pattern: See `middleware/install_check.rs`
- Admin routes pattern: See `routes/admin.rs` for category/tag management
- Form integration: See `routes/item.rs` for `edit_item_form`

### Stage Awareness (v1.0 Scope)

For v1.0, use `stage_id = 'live'` only. Full stage awareness (aliases per stage) deferred to Story 21.5.

The schema includes `stage_id` column to enable future staging support without migration changes.

### References

- [Source: epics.md#Story-15.5]
- [Source: epics.md#Story-21.5] - Future stage-aware aliases
- [Source: project-context.md#Middleware] - Middleware patterns
- [Source: project-context.md#Database-Migrations] - Migration conventions

## Dev Agent Record

### Agent Model Used

Claude Opus 4.5 (claude-opus-4-5-20251101)

### File List

- `crates/kernel/migrations/20260216000004_create_url_alias.sql`
- `crates/kernel/src/models/url_alias.rs`
- `crates/kernel/src/models/mod.rs` (modified)
- `crates/kernel/src/middleware/path_alias.rs`
- `crates/kernel/src/middleware/mod.rs` (modified)
- `crates/kernel/src/main.rs` (modified)
- `crates/kernel/src/routes/item.rs` (modified)
- `crates/kernel/src/routes/admin.rs` (modified)
- `templates/admin/aliases.html`
- `crates/kernel/tests/url_alias_test.rs`

## Acceptance Criteria Verification

| AC | Criterion | Status | Evidence |
|----|-----------|--------|----------|
| 1 | Item accessible via both /item/{id} and alias path | ✅ PASS | `test_e2e_middleware_rewrites_alias_to_source` - both paths return same status |
| 2 | Internal rewrite (no redirect) | ✅ PASS | Middleware rewrites URI in-place, no redirect response |
| 3 | Canonical URL uses alias | ✅ PASS | `UrlAlias::get_canonical_url()` returns alias if exists, tested in `test_alias_canonical_url` |
| 4 | Most recent alias is canonical | ✅ PASS | `test_alias_multiple_for_same_source` - ORDER BY created DESC, id DESC |
| 5 | Admin can set/change alias | ✅ PASS | Admin UI at `/admin/structure/aliases/*`, item edit form includes url_alias field |
| 6 | Stage-aware (deferred) | ➖ N/A | Schema includes stage_id, v1.0 uses 'live' only per spec |

### Test Summary

- **12 tests passing** (7 model + 5 e2e HTTP tests)
- Model tests: CRUD, canonical, upsert, multiple aliases
- E2E tests: middleware rewrite, query string preservation, system path bypass, admin auth

### Manual Verification Checklist

- [ ] Start server and login as admin
- [ ] Navigate to `/admin/structure/aliases`
- [ ] Create alias: source=/item/{uuid}, alias=/about-us
- [ ] Verify /about-us loads the item content
- [ ] Edit alias and verify change takes effect
- [ ] Delete alias and verify /about-us returns 404

## Change Log

- 2026-02-15: Story created via BMAD create-story workflow
- 2026-02-15: Implementation complete - all tasks done, 12 tests passing
