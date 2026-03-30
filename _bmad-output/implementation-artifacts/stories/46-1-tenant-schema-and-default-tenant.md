# Story 46.1: Tenant Schema and Default Tenant

Status: ready-for-dev

## Story

As a **kernel maintaining multi-tenancy infrastructure**,
I want a tenant table and tenant_id foreign keys on content tables,
so that the database schema supports tenant isolation from day one.

## Acceptance Criteria

1. Migration creates `tenant` table with columns: `id` (UUID PK), `name` (VARCHAR(255) NOT NULL), `machine_name` (VARCHAR(128) UNIQUE NOT NULL), `status` (BOOLEAN DEFAULT TRUE), `created` (BIGINT), `data` (JSONB DEFAULT '{}')
2. A `DEFAULT_TENANT_ID` constant UUID is defined in `crates/kernel/src/config.rs`
3. Migration seeds the default tenant row using `DEFAULT_TENANT_ID`
4. Migration adds `tenant_id` (UUID NOT NULL DEFAULT DEFAULT_TENANT_ID, FK to tenant.id, indexed) to: `item`, `item_revision`, `categories`, `category_tag`, `file_managed`, `site_config`, `url_alias`, `stage`, `menu_link`, `tile`, `comments`
5. Existing rows in all affected tables are backfilled with `DEFAULT_TENANT_ID`
6. A `Tenant` model struct is defined in `crates/kernel/src/models/tenant.rs` with CRUD methods
7. At least 2 integration tests: one verifying tenant CRUD, one verifying that existing content queries still work with the default tenant

## Tasks / Subtasks

- [ ] Define `DEFAULT_TENANT_ID` constant UUID in `crates/kernel/src/config.rs` (AC: #2)
- [ ] Write migration SQL to create `tenant` table (AC: #1)
- [ ] Write migration SQL to seed the default tenant row (AC: #3)
- [ ] Write migration SQL to add `tenant_id` column with FK and index to each affected table (AC: #4)
- [ ] Write migration SQL to backfill existing rows with `DEFAULT_TENANT_ID` (AC: #5)
- [ ] Create `crates/kernel/src/models/tenant.rs` with `Tenant` struct and CRUD methods (AC: #6)
- [ ] Register `tenant` module in `crates/kernel/src/models/mod.rs` (AC: #6)
- [ ] Write integration test for tenant CRUD operations (AC: #7)
- [ ] Write integration test verifying existing content queries work with default tenant (AC: #7)

## Dev Notes

### Architecture

The `DEFAULT_TENANT_ID` should be a well-known UUID (e.g., a v5 UUID derived from a namespace). All existing data is assigned to this tenant via `DEFAULT` clause and backfill, ensuring zero behavioral change for single-site deployments. The `tenant_id` column default means existing INSERT statements that don't specify a tenant continue to work.

Migration ordering matters: the `tenant` table and seed row must exist before adding FKs on other tables. Use multiple migration files if needed, ordered by timestamp.

### Security

Tenant isolation is enforced at the query level in subsequent stories (46.4). This story only adds the schema -- it does not yet filter queries by tenant.

### Testing

- Tenant CRUD test: create, read, update, delete a tenant via model methods.
- Backward compatibility test: create an item without specifying `tenant_id`, verify it defaults to `DEFAULT_TENANT_ID` and can be queried normally.

### References

- `crates/kernel/src/config.rs` -- configuration constants
- `crates/kernel/src/models/` -- existing model pattern
- Existing migration files for column-addition patterns
