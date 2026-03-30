# Story 46.4: Gather Auto-Filter by Tenant

Status: ready-for-dev

## Story

As a **kernel executing Gather queries**,
I want all queries automatically filtered by the current tenant,
so that cross-tenant data leakage is impossible through normal query paths.

## Acceptance Criteria

1. The Gather query builder injects `WHERE tenant_id = $tenant_id` on the item table for every query
2. Tenant filter injection happens at the builder level and is not visible as a user-configurable filter
3. The Gather admin UI does not expose a tenant filter control
4. Category queries are also filtered by `tenant_id`
5. URL alias lookups are filtered by `tenant_id`
6. Admin listing queries (item lists, category lists) are filtered by `tenant_id`
7. API queries verify the requested item belongs to the current tenant; return 404 if not
8. Cross-tenant queries are not possible through any normal request path
9. At least 3 integration tests: Gather query filtering, API item tenant check, URL alias tenant scoping

## Tasks / Subtasks

- [ ] Modify Gather query builder to inject `tenant_id` condition on item table (AC: #1, #2)
- [ ] Ensure Gather admin UI does not render a tenant filter option (AC: #3)
- [ ] Add `tenant_id` filter to category listing queries (AC: #4)
- [ ] Add `tenant_id` filter to URL alias lookup queries (AC: #5)
- [ ] Add `tenant_id` filter to admin listing routes (items, categories, etc.) (AC: #6)
- [ ] Add tenant ownership check in API item endpoints: load item, verify `tenant_id` matches request tenant, return 404 on mismatch (AC: #7)
- [ ] Audit all content query paths for missing tenant filters (AC: #8)
- [ ] Write integration test: Gather query only returns items from the current tenant (AC: #9)
- [ ] Write integration test: API item request for wrong-tenant item returns 404 (AC: #9)
- [ ] Write integration test: URL alias resolves only within the current tenant (AC: #9)

## Dev Notes

### Architecture

The tenant filter injection should mirror the stage filtering pattern already used in Gather queries. The `tenant_id` is extracted from `TenantContext` in request extensions. For single-tenant deployments, the filter still applies but always matches `DEFAULT_TENANT_ID`, which is the only value in the column.

The item API endpoints (`/api/item/{id}`, `/item/{id}`) must verify `item.tenant_id == request_tenant_id` after loading. This is defense-in-depth: even if an attacker guesses a UUID, they cannot access cross-tenant items.

### Security

- This is the critical tenant isolation story. Every query path that returns content must filter by tenant.
- The 404 response for wrong-tenant items (not 403) prevents tenant enumeration.
- Audit: grep for all `SELECT` queries on tenant-scoped tables to ensure none bypass the filter.

### Testing

- Create items in two tenants. Execute a Gather query in tenant A's context. Verify only tenant A items are returned.
- Request an item by UUID that belongs to tenant B while in tenant A's context. Verify 404.
- Create the same alias path in two tenants. Verify each resolves to its own tenant's item.

### References

- `crates/kernel/src/gather/` -- Gather query builder (stage filtering pattern)
- `crates/kernel/src/routes/gather_routes.rs` -- Gather route handlers
- `crates/kernel/src/routes/api_v1.rs` -- API item endpoints
