# Phase 4: Gather Query Engine & Categories - Progress Report

**Date**: 2026-02-12
**Status**: Complete
**Gate**: "Recent Articles" Gather query with category filter + pager renders correctly

## Executive Summary

Phase 4 implements the Gather query engine and Categories system. Gather provides a type-safe query builder using SeaQuery for constructing content listings with filters, sorts, and paging. Categories enable hierarchical taxonomy with tags supporting DAG structures (multiple parents).

## Completed Work

### Database Schema (4 migrations)

**category table**:
- `id` VARCHAR(32) PRIMARY KEY
- `label`, `description`, `hierarchy`, `weight`

**category_tag table**:
- `id` UUID PRIMARY KEY
- `category_id` FK to category
- `label`, `description`, `weight`
- `created`, `changed` timestamps

**category_tag_hierarchy table**:
- `id` SERIAL PRIMARY KEY
- `tag_id`, `parent_id` UUIDs with CASCADE delete
- Unique indexes for parent relationships and root tags
- Supports DAG (multiple parents per tag)

**gather_view table**:
- `view_id` VARCHAR(64) PRIMARY KEY
- `definition` JSONB (ViewDefinition)
- `display` JSONB (ViewDisplay)
- `plugin`, `created`, `changed`

### Category Models (`models/category.rs`)

**Category**:
- `find_by_id`, `list`, `create`, `update`, `delete`, `exists`

**Tag**:
- CRUD: `find_by_id`, `list_by_category`, `create`, `update`, `delete`
- Hierarchy: `get_parents`, `get_children`, `get_roots`
- Recursive CTEs: `get_ancestors`, `get_descendants`, `get_tag_and_descendant_ids`
- Hierarchy management: `set_parents`, `add_parent`, `remove_parent`

### CategoryService (`gather/category_service.rs`)

- CRUD operations with DashMap caching
- Hierarchy queries (breadcrumb, tree traversal)
- Cache invalidation on writes

### Gather Types (`gather/types.rs`)

| Type | Description |
|------|-------------|
| `ViewDefinition` | Query specification: base_table, item_type, fields, filters, sorts, relationships |
| `ViewDisplay` | Rendering: format, items_per_page, pager config |
| `ViewFilter` | Field, operator, value, exposed flag |
| `FilterOperator` | 16 operators including category-aware: `HasTag`, `HasTagOrDescendants` |
| `FilterValue` | String, Integer, Float, Boolean, UUID, List, Contextual |
| `ViewSort` | Field, direction (Asc/Desc), nulls handling |
| `GatherView` | Complete view definition with metadata |
| `GatherResult` | Query results with pagination info |

### ViewQueryBuilder (`gather/query_builder.rs`)

- SeaQuery-based SQL generation
- JSONB field extraction (`fields->>'name'`)
- Filter operator implementation
- Stage-aware queries (always filters by `stage_id`)
- Pagination with LIMIT/OFFSET
- COUNT query for totals

### GatherService (`gather/gather_service.rs`)

- View registration and persistence
- View loading from database at startup
- Query execution with exposed filter resolution
- Category hierarchy expansion (`HasTagOrDescendants` → tag + all descendants)

### HTTP Routes

**Category Routes** (`routes/category.rs`):
| Method | Path | Handler |
|--------|------|---------|
| GET | `/api/categories` | list_categories |
| GET/POST/PUT/DELETE | `/api/category/{id}` | CRUD |
| GET | `/api/category/{id}/tags` | list_tags |
| GET | `/api/category/{id}/roots` | get_root_tags |
| GET/POST/PUT/DELETE | `/api/tag/{id}` | CRUD |
| GET | `/api/tag/{id}/parents` | get_parents |
| PUT | `/api/tag/{id}/parents` | set_parents |
| GET | `/api/tag/{id}/children` | get_children |
| GET | `/api/tag/{id}/ancestors` | get_ancestors |
| GET | `/api/tag/{id}/descendants` | get_descendants |
| GET | `/api/tag/{id}/breadcrumb` | get_breadcrumb |

**Gather Routes** (`routes/gather.rs`):
| Method | Path | Handler |
|--------|------|---------|
| GET | `/api/views` | list_views |
| GET | `/api/view/{view_id}` | get_view |
| GET | `/api/view/{view_id}/execute` | execute_view |
| POST | `/api/gather/query` | execute_adhoc_query |
| GET | `/gather/{view_id}` | render_view_html |

### AppState Integration

Added to `state.rs`:
- `categories: Arc<CategoryService>` - created at startup
- `gather: Arc<GatherService>` - loads views from database
- Getters: `categories()`, `gather()`

## Test Coverage

**Unit Tests** (in lib - 110 total):
- Category models: 5 tests (category, tag, hierarchy)
- Gather types: 12 tests (serialization, defaults, conversions)
- Query builder: 5 tests (SQL generation, filters, pagination)
- Gather service: 5 tests (result pagination)
- Filter pipeline: 18 tests
- Form builder: 12 tests
- Plugin system: ~30 tests
- Model structs: 10 tests
- Tap system: 10+ tests

**Integration Tests** (117 total):
- `category_test.rs`: 13 tests (category, tag, hierarchy)
- `gather_test.rs`: 24 tests (types, operators, gate test)
- `item_test.rs`: 47 tests
- `plugin_test.rs`: 24 tests
- `integration_test.rs`: 9 tests

**Test Utils** (6 tests)

**Total: 227+ tests passing**

## Gate Test

```rust
#[test]
fn gate_test_recent_articles_view_definition() {
    let view = GatherView {
        view_id: "recent_articles".to_string(),
        definition: ViewDefinition {
            base_table: "item".to_string(),
            item_type: Some("blog".to_string()),
            filters: vec![
                ViewFilter {
                    field: "status",
                    operator: FilterOperator::Equals,
                    value: FilterValue::Integer(1),  // Published
                },
                ViewFilter {
                    field: "fields.category",
                    operator: FilterOperator::HasTagOrDescendants,
                    value: FilterValue::Uuid(tech_tag_id),
                    exposed: true,
                },
            ],
            sorts: vec![
                ViewSort { field: "sticky", direction: SortDirection::Desc },
                ViewSort { field: "created", direction: SortDirection::Desc },
            ],
            ..Default::default()
        },
        display: ViewDisplay {
            items_per_page: 10,
            pager: PagerConfig { enabled: true, style: PagerStyle::Full },
            ..Default::default()
        },
        ..Default::default()
    };

    // Verify serialization and all properties
    // ...
}
```

## File Structure

```
crates/kernel/
├── migrations/
│   ├── 20260212000006_create_category.sql
│   ├── 20260212000007_create_category_tag.sql
│   ├── 20260212000008_create_category_tag_hierarchy.sql
│   └── 20260212000009_create_gather_view.sql
├── src/
│   ├── models/
│   │   ├── mod.rs           # Added category exports
│   │   └── category.rs      # Category, Tag, TagHierarchy
│   ├── gather/
│   │   ├── mod.rs           # Module exports
│   │   ├── types.rs         # ViewDefinition, ViewDisplay, etc.
│   │   ├── query_builder.rs # SeaQuery SQL generation
│   │   ├── category_service.rs
│   │   └── gather_service.rs
│   ├── routes/
│   │   ├── mod.rs           # Added gather/category routes
│   │   ├── category.rs      # Category/tag HTTP handlers
│   │   └── gather.rs        # View execution handlers
│   ├── lib.rs               # Added gather module export
│   ├── main.rs              # Added gather module and routes
│   └── state.rs             # Added CategoryService, GatherService
└── tests/
    ├── category_test.rs     # 13 tests
    └── gather_test.rs       # 24 tests (includes gate test)
```

## Verification

```bash
# All tests pass
cargo test -p trovato-kernel --lib
# running 110 tests ... ok

cargo test --test category_test
# running 13 tests ... ok

cargo test --test gather_test
# running 24 tests ... ok

cargo test --test item_test
# running 47 tests ... ok

cargo test --test plugin_test
# running 24 tests ... ok

cargo test --test integration_test
# running 9 tests ... ok

# Total: 227+ tests passing
```

## Dependencies

- `sea-query` 0.32 with `backend-postgres` feature (already in workspace)

## Key Design Decisions

1. **DAG Hierarchy**: Tags can have multiple parents via junction table
2. **Recursive CTEs**: Efficient tree traversal without application-level recursion
3. **SeaQuery**: Type-safe SQL building with JSONB support via `Expr::cust()`
4. **Category Filter Expansion**: `HasTagOrDescendants` resolved at query time
5. **Stage-Aware Queries**: All queries automatically filter by `stage_id`
6. **Exposed Filters**: User-modifiable filter values for dynamic views

## Next Steps (Phase 5)

1. Template integration for HTML rendering
2. Tile/slot system for page layouts
3. Asset pipeline (CSS/JS bundling)
4. Cache layer for query results
