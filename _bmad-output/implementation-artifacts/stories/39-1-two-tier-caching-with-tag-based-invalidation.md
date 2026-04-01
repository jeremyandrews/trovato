# Story 39.1: Two-Tier Caching with Tag-Based Invalidation

Status: done

## Story

As a **platform operator**,
I want a two-tier cache with fast in-process lookups and shared cross-instance storage,
so that the system scales horizontally without cache stampede or stale data across instances.

## Acceptance Criteria

1. Two-tier cache implemented: Moka L1 (in-process, 60s TTL, 10K entry cap) and Redis L2 (shared, 5min TTL)
2. Cache reads check L1 first, then L2; L2 hits populate L1 automatically
3. Tag-based invalidation via Redis Lua script atomically removes all keys associated with a tag
4. Single-key invalidation clears both L1 and L2
5. Stage-scoped cache keys: live stage uses bare keys, non-live stages use `st:{stage_id}:{key}` prefix
6. TTL is configurable per cache entry via the `set()` method
7. Graceful degradation: Redis connection failures log warnings and fall back to L1-only operation

## Tasks / Subtasks

- [x] Define `CacheLayer` struct wrapping `Arc<CacheLayerInner>` with Moka L1 and Redis L2 (AC: #1)
- [x] Implement `CacheLayer::new()` with Moka builder (10K max capacity, 60s TTL) and Redis client (AC: #1)
- [x] Implement `get()` with L1-first, L2-fallback, L1-populate-on-miss pattern (AC: #2)
- [x] Implement `set()` writing to both tiers with configurable TTL and tag registration via Redis SADD (AC: #1, #6)
- [x] Implement `invalidate()` for single-key removal from both L1 and L2 (AC: #4)
- [x] Implement `invalidate_tag()` using Redis Lua script for atomic key-set deletion (AC: #3)
- [x] Implement `stage_key()` static method for stage-scoped key generation (AC: #5)
- [x] Add graceful Redis failure handling with `warn!` logging at all Redis call sites (AC: #7)
- [x] Add unit tests for cache operations (AC: #1, #2, #3)

## Dev Notes

### Architecture

The `CacheLayer` is a `Clone`-able handle backed by `Arc<CacheLayerInner>`. L1 uses `moka::future::Cache<String, String>` for async-safe in-process caching. L2 uses Redis `SET EX` for TTL-managed entries and `SADD` to associate keys with invalidation tags. The Lua invalidation script (`INVALIDATE_TAG_SCRIPT`) fetches all members of a tag set, deletes the keys, then deletes the tag set itself -- all atomically.

Stage scoping uses a simple key-prefix strategy: the live stage (well-known UUID) maps to bare keys for maximum cache hit rates, while non-live stages get `st:{uuid}:{key}` prefixes to isolate preview/draft data.

### Testing

- Unit tests in `crates/kernel/src/cache/mod.rs` (2 tests)
- Integration tests in `crates/kernel/tests/cache_test.rs` (4 tests)
- Tests require a running Redis instance

### References

- `crates/kernel/src/cache/mod.rs` (330 lines) -- CacheLayer, Lua script, stage_key
- `crates/kernel/src/cache/service.rs` -- service-level cache wrappers
- `crates/kernel/src/cache/types.rs` -- cache configuration types
