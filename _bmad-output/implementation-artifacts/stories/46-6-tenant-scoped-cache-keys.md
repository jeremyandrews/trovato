# Story 46.6: Tenant-Scoped Cache Keys

Status: ready-for-dev

## Story

As a **kernel maintaining cache isolation**,
I want cache keys prefixed with the tenant ID,
so that cached data from one tenant never leaks to another.

## Acceptance Criteria

1. Cache key format becomes `t:{tenant_id}:st:{stage_id}:{key}` for multi-tenant deployments
2. Single-tenant optimization: when tenant is `DEFAULT_TENANT_ID`, cache key format remains `st:{stage_id}:{key}` (backward compatible, no key migration needed)
3. Tag-based cache invalidation is scoped by tenant (invalidating tenant A's cache does not affect tenant B)
4. All cache services are updated: `ContentTypeRegistry`, `GatherService`, `PermissionService`, `UserService`, `ItemService`, `CategoryService`
5. Redis Lua scripts (if any) are updated to handle the new key format
6. At least 2 integration tests: tenant-scoped cache isolation, single-tenant backward-compatible keys

## Tasks / Subtasks

- [ ] Create a cache key builder utility that prepends tenant prefix when not `DEFAULT_TENANT_ID` (AC: #1, #2)
- [ ] Update `ContentTypeRegistry` cache keys to use tenant-scoped format (AC: #4)
- [ ] Update `GatherService` cache keys to use tenant-scoped format (AC: #4)
- [ ] Update `PermissionService` cache keys to use tenant-scoped format (AC: #4)
- [ ] Update `UserService` cache keys to use tenant-scoped format (AC: #4)
- [ ] Update `ItemService` cache keys to use tenant-scoped format (AC: #4)
- [ ] Update `CategoryService` cache keys to use tenant-scoped format (AC: #4)
- [ ] Update tag-based invalidation to scope by tenant (AC: #3)
- [ ] Update Redis Lua scripts for new key format (AC: #5)
- [ ] Write integration test: same key in two tenants caches independently (AC: #6)
- [ ] Write integration test: `DEFAULT_TENANT_ID` uses backward-compatible key format (AC: #6)

## Dev Notes

### Architecture

The cache key builder should be a small utility function:
```
fn cache_key(tenant_id: Uuid, stage_id: Uuid, key: &str) -> String
```
When `tenant_id == DEFAULT_TENANT_ID`, it omits the `t:` prefix for backward compatibility. This means existing single-tenant deployments experience no cache key changes and no cache invalidation on upgrade.

Each `moka::sync::Cache` instance is per-process, so tenant scoping is achieved purely through key prefixing. For Redis-backed caches, the prefix also ensures key namespace isolation.

### Testing

- Insert a value under key "items:conference" for tenant A and tenant B. Verify fetching from tenant A returns tenant A's value.
- Insert a value using `DEFAULT_TENANT_ID`. Verify the actual cache key does not contain the `t:` prefix.

### References

- `crates/kernel/src/cache/` -- cache infrastructure
- `crates/kernel/src/services/` -- service caches (moka)
- `crates/kernel/src/config.rs` -- `DEFAULT_TENANT_ID` constant
