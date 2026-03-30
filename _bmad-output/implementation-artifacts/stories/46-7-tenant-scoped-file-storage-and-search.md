# Story 46.7: Tenant-Scoped File Storage and Search

Status: ready-for-dev

## Story

As a **kernel maintaining data isolation**,
I want file storage and search scoped by tenant,
so that tenants cannot access each other's uploaded files or search results.

## Acceptance Criteria

1. Multi-tenant file URI format: `local://{tenant_machine_name}/YYYY/MM/{uuid}_{filename}`
2. Single-tenant file URI format remains: `local://YYYY/MM/{uuid}_{filename}` (backward compatible)
3. File serve routes verify the requested file belongs to the current tenant before serving
4. S3 storage backend uses tenant machine name as key prefix
5. Full-text search (`tsvector`) queries filter by `tenant_id`
6. Pagefind index is scoped per tenant (separate indexes per tenant)
7. At least 2 integration tests: file tenant isolation, search tenant isolation

## Tasks / Subtasks

- [ ] Update file storage service to include tenant machine name in local file paths for multi-tenant mode (AC: #1)
- [ ] Preserve existing path format when tenant is `DEFAULT_TENANT_ID` (AC: #2)
- [ ] Add tenant ownership check in file serve route: verify `file_managed.tenant_id` matches request tenant (AC: #3)
- [ ] Update S3 storage key generation to prefix with tenant machine name (AC: #4)
- [ ] Add `tenant_id` filter to full-text search queries (AC: #5)
- [ ] Implement per-tenant Pagefind index generation (AC: #6)
- [ ] Write integration test: upload file in tenant A, attempt to serve from tenant B context, verify denied (AC: #7)
- [ ] Write integration test: search in tenant A returns only tenant A items (AC: #7)

## Dev Notes

### Architecture

File storage isolation uses the tenant's `machine_name` as a directory prefix. This provides both logical isolation (different paths) and operational convenience (easy to identify which tenant owns which files on disk).

The backward-compatible path for `DEFAULT_TENANT_ID` means existing single-tenant installations need no file migration. The file serve route checks `file_managed.tenant_id` against the request's `TenantContext`, which is defense-in-depth beyond path-based isolation.

Pagefind indexes are generated per tenant into separate directories (e.g., `pagefind/{tenant_machine_name}/`). The search route selects the correct index based on the current tenant.

### Security

- File serve route must verify tenant ownership even if the file path contains the correct tenant prefix (defense-in-depth).
- S3 bucket policies can optionally enforce tenant isolation at the IAM level, but application-level checks are mandatory.

### Testing

- Upload a file as tenant A. Switch to tenant B's context. Attempt to serve the file by its UUID. Verify 404 or 403.
- Create items with searchable content in two tenants. Search from tenant A. Verify only tenant A results appear.

### References

- `crates/kernel/src/services/` -- file storage service
- `crates/kernel/src/search/` -- search indexing and query
- `crates/kernel/src/routes/` -- file serve routes
