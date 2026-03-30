# Story 46.3: Tenant Resolution Middleware

Status: ready-for-dev

## Story

As a **kernel processing multi-tenant requests**,
I want tenant resolution early in the middleware pipeline,
so that all downstream handlers know which tenant the request is for.

## Acceptance Criteria

1. Tenant resolution middleware runs after session/auth middleware and before route handlers
2. Supported resolution strategies: subdomain (`tenant-a.example.com` resolves to machine_name `tenant-a`), path prefix (`/t/tenant-a/...` strips prefix and resolves), header (`X-Tenant-ID: uuid`), default (always returns `DEFAULT_TENANT_ID`)
3. `TENANT_RESOLUTION_METHOD` env var selects the strategy; defaults to `"default"`
4. Resolved tenant is stored as `TenantContext` in `request.extensions()` via `request.extensions().get::<TenantContext>()`
5. `TenantContext` struct contains: `id: Uuid`, `name: String`, `machine_name: String`
6. Tenant context is cached per request (resolved once, not re-computed)
7. The "default" strategy returns immediately with zero overhead (no database lookup)
8. At least 3 integration tests: default strategy, header strategy, invalid tenant returns error

## Tasks / Subtasks

- [ ] Create `crates/kernel/src/middleware/tenant.rs` module (AC: #1)
- [ ] Define `TenantContext` struct with `id`, `name`, `machine_name` fields (AC: #5)
- [ ] Implement `TenantResolutionStrategy` enum with variants: `Default`, `Subdomain`, `PathPrefix`, `Header` (AC: #2)
- [ ] Implement subdomain resolution: extract subdomain from `Host` header, look up tenant by `machine_name` (AC: #2)
- [ ] Implement path prefix resolution: match `/t/{machine_name}/...`, strip prefix, resolve tenant (AC: #2)
- [ ] Implement header resolution: read `X-Tenant-ID` header, look up tenant by UUID (AC: #2)
- [ ] Implement default resolution: return `DEFAULT_TENANT_ID` context immediately without DB lookup (AC: #2, #7)
- [ ] Read `TENANT_RESOLUTION_METHOD` from config/env, default to `"default"` (AC: #3)
- [ ] Insert `TenantContext` into request extensions (AC: #4, #6)
- [ ] Register middleware in the pipeline after auth, before routes (AC: #1)
- [ ] Write integration test for default strategy (AC: #8)
- [ ] Write integration test for header strategy (AC: #8)
- [ ] Write integration test for invalid/unknown tenant returning error (AC: #8)

## Dev Notes

### Architecture

The middleware is an Axum layer. For the "default" strategy, the middleware constructs a static `TenantContext` from the `DEFAULT_TENANT_ID` constant and the seeded tenant row's name/machine_name -- no database query needed. This is the zero-overhead path for single-tenant deployments.

For multi-tenant strategies, the middleware looks up the tenant in the database (or a cache). The path-prefix strategy must also rewrite the URI to strip `/t/{machine_name}` before passing to route handlers.

### Security

- Subdomain strategy: validate that the subdomain matches `is_valid_machine_name()` before database lookup.
- Header strategy: intended for API/internal use, not public-facing. Consider restricting to trusted origins.
- Invalid tenant should return 404 (tenant not found), not 500.

### Testing

- Default: make a request with no special headers/paths, verify `TenantContext` is `DEFAULT_TENANT_ID`.
- Header: set `X-Tenant-ID` to a known tenant UUID, verify resolution.
- Invalid: set `X-Tenant-ID` to a nonexistent UUID, verify error response.

### References

- `crates/kernel/src/middleware/` -- existing middleware modules
- `crates/kernel/src/config.rs` -- `DEFAULT_TENANT_ID` constant (from 46.1)
- `crates/kernel/src/routes/helpers.rs` -- `is_valid_machine_name()`
