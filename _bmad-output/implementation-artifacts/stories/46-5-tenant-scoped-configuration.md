# Story 46.5: Tenant-Scoped Configuration

Status: ready-for-dev

## Story

As a **site operator with multiple tenants**,
I want site configuration scoped per tenant,
so that each tenant can have independent settings without affecting others.

## Acceptance Criteria

1. `ConfigStorage` trait methods gain a `tenant_id: Uuid` parameter
2. `site_config` queries filter by `tenant_id`
3. Config export exports only the current tenant's configuration
4. Config import imports into the current tenant's scope
5. `StageAwareConfigStorage` becomes `TenantStageAwareConfigStorage` (or gains tenant awareness)
6. Configuration resolution chain: tenant-specific value -> default tenant value -> hardcoded defaults
7. In single-tenant mode, `DEFAULT_TENANT_ID` is always used with no behavioral change from current behavior
8. At least 2 integration tests: tenant-specific config isolation, resolution chain fallback

## Tasks / Subtasks

- [ ] Add `tenant_id: Uuid` parameter to `ConfigStorage` trait methods (AC: #1)
- [ ] Update `DirectConfigStorage` implementation to filter by `tenant_id` (AC: #1, #2)
- [ ] Update config export to scope by current tenant (AC: #3)
- [ ] Update config import to write into current tenant's scope (AC: #4)
- [ ] Rename or extend `StageAwareConfigStorage` to include tenant awareness (AC: #5)
- [ ] Implement resolution chain: tenant-specific -> default tenant -> hardcoded defaults (AC: #6)
- [ ] Ensure single-tenant path always passes `DEFAULT_TENANT_ID` with no behavior change (AC: #7)
- [ ] Update all `ConfigStorage` call sites to pass `tenant_id` (AC: #1)
- [ ] Write integration test: set config in tenant A, verify tenant B does not see it (AC: #8)
- [ ] Write integration test: verify fallback chain (tenant-specific -> default -> hardcoded) (AC: #8)

## Dev Notes

### Architecture

The resolution chain enables a pattern where the default tenant's config serves as a template. Tenant-specific overrides take precedence, but unconfigured values fall through to the default tenant's settings, and finally to hardcoded defaults. This reduces configuration burden for multi-tenant deployments.

The `tenant_id` parameter flows from `TenantContext` in request extensions through service methods to the storage layer.

### Testing

- Create two tenants. Set a config value in tenant A. Query from tenant B's context. Verify the value is not visible.
- Set a value only in the default tenant. Query from tenant A (which has no override). Verify the default tenant's value is returned.

### References

- `crates/kernel/src/config_storage/mod.rs` -- `ConfigStorage` trait
- `crates/kernel/src/config_storage/direct.rs` -- `DirectConfigStorage`
- `crates/kernel/src/models/site_config.rs` -- site configuration model
