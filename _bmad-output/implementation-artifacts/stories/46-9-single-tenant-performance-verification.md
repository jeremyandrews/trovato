# Story 46.9: Single-Tenant Performance Verification

Status: ready-for-dev

## Story

As a site operator running a single-tenant Trovato installation,
I want verified evidence that multi-tenancy infrastructure adds negligible overhead,
so that I can trust that the tenant_id columns, middleware, and scoped queries don't degrade my site's performance.

## Acceptance Criteria

1. Benchmark: tenant resolution middleware adds <0.1ms per request in "default" strategy mode
2. Benchmark: Gather query with tenant_id WHERE clause adds <1ms compared to query without it (on a 10K item dataset)
3. Benchmark: cache key generation with DEFAULT_TENANT_ID optimization matches pre-tenant key generation time
4. Single-tenant sites use the backward-compatible cache key format (no tenant prefix) — verified by comparing keys before and after the migration
5. No measurable increase in memory usage from tenant middleware on single-tenant installations
6. All benchmarks documented in operational docs with methodology and baseline numbers
7. At least 3 benchmark tests covering middleware, query, and cache key paths

## Tasks / Subtasks

- [ ] Create benchmark harness for tenant resolution middleware (AC: #1)
  - [ ] Measure "default" strategy resolution time over 10K requests
  - [ ] Compare to baseline (no middleware)
- [ ] Create benchmark for Gather queries with tenant_id (AC: #2)
  - [ ] Seed 10K items with DEFAULT_TENANT_ID
  - [ ] Run identical Gather query with and without tenant_id WHERE clause
  - [ ] Measure p50, p95, p99 latencies
- [ ] Verify cache key backward compatibility (AC: #3, #4)
  - [ ] Assert DEFAULT_TENANT_ID produces same cache key format as pre-tenant code
- [ ] Measure memory footprint delta (AC: #5)
  - [ ] Compare RSS before and after tenant middleware is added to pipeline
- [ ] Document all benchmark results (AC: #6)
- [ ] Write benchmark integration tests (AC: #7)

## Dev Notes

### Architecture

This is a verification story, not an implementation story. It runs after Stories 46.1–46.8 are complete. If benchmarks fail the thresholds, the fixes are in the relevant implementation stories (46.3 for middleware, 46.4 for queries, 46.6 for cache), not here.

The "default" strategy must be zero-allocation: construct a static `TenantContext` from compile-time constants, not from a database lookup. If the current implementation does a DB query for the default tenant, that's a bug this story catches.

### Testing

- Use `criterion` or equivalent for micro-benchmarks
- Use `pg_stat_statements` for query timing comparison
- Single-tenant baseline must be captured before multi-tenancy code merges (or by temporarily disabling it)

### References

- `crates/kernel/src/middleware/tenant.rs` — tenant resolution middleware
- `crates/kernel/src/gather/` — query builder with tenant filtering
- `crates/kernel/src/cache/` — cache key generation
- [Source: docs/ritrovo/epic-16-multi-tenancy.md]
