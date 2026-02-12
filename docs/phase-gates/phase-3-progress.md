# Phase 3: Content System - Progress Report

**Date**: 2026-02-12
**Status**: In Progress
**Stories Completed**: 7 of 11 (stories 4.1-4.7, partial 4.8-4.11)

## Executive Summary

Phase 3 implements the core content management system based on Epic 4 (Content Modeling & Basic CRUD). The foundation is complete with database schema, models, services, routes, filters, and auto-generated forms.

## Completed Work

### Database Schema (Stories 4.1, 4.2)

Created migrations for content tables:

**item_type table** (Story 4.1):
- `type` VARCHAR(32) PRIMARY KEY
- `label`, `description`, `has_title`, `title_label`
- `plugin` - owning plugin
- `settings` JSONB - field definitions
- Seeded "page" default type

**item table** (Story 4.2):
- `id` UUID PRIMARY KEY (UUIDv7)
- `current_revision_id` UUID FK
- `type`, `title`, `author_id`, `status`
- `created`, `changed` (BIGINT timestamps)
- `promote`, `sticky` (flags)
- `fields` JSONB with GIN index
- `search_vector` tsvector for full-text search
- `stage_id` for content staging

**item_revision table** (Story 4.2):
- `id` UUID PRIMARY KEY
- `item_id` UUID FK with CASCADE delete
- `author_id`, `title`, `status`, `fields`
- `created`, `log` (revision message)

### Models (Stories 4.1, 4.2)

**ItemType model** (`models/item_type.rs`):
- `find_by_type`, `list`, `list_by_plugin`
- `create`, `upsert`, `delete`
- `exists` check

**Item model** (`models/item.rs`):
- Full CRUD: `find_by_id`, `create`, `update`, `delete`
- Listing: `list_by_type`, `list_by_author`, `list_published`
- Revisions: `get_revisions`, `get_revision`, `revert_to_revision`
- Counters: `count_by_type`, `count_published`
- All operations use transactions for consistency

### Content Type Registration (Story 4.3)

**ContentTypeRegistry** (`content/type_registry.rs`):
- Syncs types from plugins via `tap_item_info`
- Caches definitions in DashMap for fast access
- `get`, `list`, `type_names`, `exists` methods
- Parses field definitions from settings JSONB

### Item Service (Stories 4.4-4.7)

**ItemService** (`content/item_service.rs`):
- CRUD with automatic tap invocations
- `create()` → `tap_item_insert`
- `load()`, `load_for_view()` → `tap_item_view`
- `update()` → `tap_item_update`
- `delete()` → `tap_item_delete`
- `check_access()` → `tap_item_access` with aggregation
- In-memory caching with invalidation

### HTTP Routes (Stories 4.4-4.9, partial)

**Item routes** (`routes/item.rs`):
| Method | Path | Handler |
|--------|------|---------|
| GET | `/item/{id}` | View item |
| GET | `/item/add/{type}` | Add form |
| POST | `/item/add/{type}` | Create |
| GET | `/item/{id}/edit` | Edit form |
| POST | `/item/{id}/edit` | Update |
| POST | `/item/{id}/delete` | Delete |
| GET | `/item/{id}/revisions` | History |
| POST | `/item/{id}/revert/{rev_id}` | Revert |
| GET | `/api/content-types` | List types |
| GET | `/api/items/{type}` | List items |

### Text Format Filters (Story 4.10)

**FilterPipeline** (`content/filter.rs`):
- `plain_text` - HTML escape + newline conversion
- `filtered_html` - Allow safe tags, remove scripts/events
- `full_html` - No filtering (admin only)
- XSS protection: removes `<script>`, `<style>`, event handlers
- URL-to-link conversion

### Auto-Generated Forms (Story 4.11)

**FormBuilder** (`content/form.rs`):
- Generates HTML forms from ContentTypeDefinition
- Maps FieldType to HTML inputs:
  - Text → `<input type="text">`
  - TextLong → `<textarea>`
  - Boolean → `<input type="checkbox">`
  - Integer → `<input type="number">`
  - Date → `<input type="date">`
  - Email → `<input type="email">`
  - RecordReference → UUID input (placeholder)
  - File → `<input type="file">`
- Marks required fields
- Populates edit forms with existing values

### AppState Integration

Updated `state.rs` with Phase 3 services:
- `ContentTypeRegistry` - synced at startup
- `ItemService` - CRUD with taps
- Added getters: `content_types()`, `items()`
- Integrated with plugin loading sequence

## Test Coverage

**Unit Tests** (in lib - 84 total):
- Filter pipeline: 18 tests (HtmlEscape, Newline, FilteredHtml, UrlFilter, pipelines)
- Form builder: 12 tests (all field types, HTML escaping, form structure)
- ItemService: 3 tests (access input serialization)
- Model structs: 5 tests
- UserContext: 5 tests
- Request state: 5 tests
- Plugin system: ~30 tests

**Integration Tests** (`tests/item_test.rs` - 47 total):
- Filter pipeline: 10 tests (edge cases, all formats)
- Form builder: 7 tests (all field types, value population, XSS protection)
- Item model: 5 tests (status checks, CRUD inputs)
- ItemType model: 1 test
- ItemRevision: 2 tests
- UserContext: 5 tests
- Request state: 2 tests
- Content type definition: 3 tests
- SDK types: 10 tests (TextValue, RecordRef, AccessResult, MenuDefinition, etc.)

**Plugin Integration Tests** (`tests/plugin_test.rs` - 24 total):
- Runtime creation: 2 tests
- Plugin loading: 3 tests
- Tap registry: 4 tests
- Tap dispatcher: 2 tests
- Menu registry: 2 tests
- Dependency resolution: 3 tests
- Error handling: 3 tests
- Host functions: 1 test
- Request state: 3 tests

**Other Integration Tests** (`tests/integration_test.rs` - 9 total)

**Test Utils** (`test-utils` - 6 tests)

Total: 170+ tests passing

## Remaining Work

### Story 4.8: Revision History View (Partial)
- Route exists: `GET /item/{id}/revisions`
- TODO: Improve UI, add diff view

### Story 4.9: Revert to Previous Revision (Partial)
- Route exists: `POST /item/{id}/revert/{rev_id}`
- TODO: Confirmation dialog, better error handling

### Story 4.10: Text Format Filter Pipeline (Complete)
- Implemented and tested

### Story 4.11: Auto-Generated Admin Forms (Complete)
- Implemented and tested

### Additional Work Needed
1. Connect to real database for E2E testing
2. Wire up permission checks to PermissionService
3. Implement tap_item_view for rendering
4. Add form validation on submission
5. Create templates with Tera for proper HTML pages

## File Structure

```
crates/kernel/
├── migrations/
│   ├── 20260212000004_create_item_types.sql
│   └── 20260212000005_create_items.sql
├── src/
│   ├── content/
│   │   ├── mod.rs
│   │   ├── filter.rs          # Text format pipeline
│   │   ├── form.rs            # Auto-generated forms
│   │   ├── item_service.rs    # CRUD + taps
│   │   └── type_registry.rs   # Content type cache
│   ├── models/
│   │   ├── item.rs            # Item, ItemRevision
│   │   └── item_type.rs       # ItemType
│   ├── routes/
│   │   └── item.rs            # HTTP handlers
│   └── state.rs               # Updated with services
└── tests/
    └── item_test.rs           # Integration tests
```

## Verification

```bash
# All tests pass
cargo test -p trovato-kernel --lib
# running 84 tests ... ok

cargo test --test item_test
# running 47 tests ... ok

cargo test --test plugin_test
# running 24 tests ... ok

cargo test --test integration_test
# running 9 tests ... ok

cargo test -p trovato-test-utils
# running 6 tests ... ok

# Total: 170+ tests passing
```

## Next Steps

1. Run migrations against actual PostgreSQL
2. Test full E2E flow with curl
3. Add Tera templates for proper HTML
4. Implement tap_item_view in blog plugin
5. Complete permission integration
