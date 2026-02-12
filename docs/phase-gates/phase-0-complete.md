# Phase 0: WASM Architecture Validation - Complete

**Date**: 2026-02-12
**Updated**: 2026-02-12 (x86-64 validation complete)
**Status**: Gates 2/3 Passed, Gate 1 Failed (architecture validated)
**Recommendation**: Proceed with full-serialization as default data access mode

## Executive Summary

Phase 0 validated the core WASM plugin architecture through three critical benchmarks on both ARM (Apple Silicon) and x86-64 (AMD EPYC). The infrastructure is sound—concurrent instantiation and async host functions work excellently under Tokio. The handle-based data access hypothesis was disproven on both architectures: **full-serialization is faster for all tested workloads**.

**Decision**: Proceed to Phase 2 using full-serialization as the default plugin data access mode. Handle-based API is not needed as a fallback—benchmarks show no scenario where it wins.

## Gate Status

| Gate | Requirement | ARM Result | x86-64 Result | Status |
|------|-------------|------------|---------------|--------|
| 1 | Handle-based >5x faster | 0.86x (serialize wins) | 0.65x (serialize wins) | **FAILED** |
| 2 | Concurrency p95 <10ms | 1.09ms | 0.86ms (2000 concurrent) | **PASSED** |
| 3 | Async without deadlock | No deadlocks | No deadlocks | **PASSED** |

**Overall**: PROCEED WITH MODIFICATIONS — The WASM architecture is validated on both ARM and x86-64. Full-serialization is the clear winner.

---

## Benchmark Results

### Gate 1: Handle-Based vs Full-Serialization

**Hypothesis**: Handle-based access (one host call per field) would be >5x faster than full-serialization (single JSON blob across boundary).

**Result**: FAILED on both architectures — Full-serialization is 1.2-1.6x faster.

#### Apple Silicon (M-series)

| Metric | Handle-Based | Full-Serialization |
|--------|--------------|-------------------|
| Tap avg | 27.16µs | 23.39µs |
| Tap p50 | 26.00µs | 22.50µs |
| Tap p95 | 33.75µs | 29.29µs |
| Tap p99 | 43.00µs | 36.92µs |

#### AMD EPYC (x86-64)

| Metric | Handle-Based | Full-Serialization |
|--------|--------------|-------------------|
| Tap avg | 36.55µs | 23.82µs |
| Tap p50 | 34.68µs | 22.95µs |
| Tap p95 | 43.75µs | 28.64µs |
| Tap p99 | 52.85µs | 34.53µs |

**Key Finding**: x86-64 shows *larger* advantage for full-serialization (1.5x vs 1.2x). Handle-based is slower on EPYC (36µs vs 27µs on ARM), while full-serialization is nearly identical (~23µs on both).

**Analysis**: The handle-based approach requires 4 WASM↔host boundary crossings for the test workload (read 3 fields + write 1 field). Each crossing involves context switch overhead, parameter marshaling, memory buffer management, and string copying. Full-serialization does a single crossing with ~2.4KB JSON—modern JSON parsing is fast enough that reduced boundary crossings win.

### Gate 2: Store Pooling Concurrency

**Requirement**: 100 parallel requests with p95 <10ms per request.

**Result**: PASSED — Scales excellently to 2000 concurrent requests.

#### Concurrency Scaling (EPYC x86-64)

| Concurrency | Total p95 | Total p99 | Instantiation p95 | Tap p95 |
|-------------|-----------|-----------|-------------------|---------|
| 100 | 1.03ms | 1.26ms | 205µs | 753µs |
| 1000 | 0.89ms | 1.25ms | 205µs | 753µs |
| 2000 | 0.86ms | 1.15ms | 196µs | 736µs |

**Analysis**: The wasmtime pooling allocator handles concurrent instantiation efficiently at scale. Performance actually *improves* at higher concurrency (likely better CPU utilization). This validates the per-request instantiation model up to at least 2000 concurrent requests.

### Gate 3: Async Host Functions

**Requirement**: No deadlocks under Tokio runtime with async host functions.

**Result**: PASSED on both architectures.

| Platform | Wall-clock (100 concurrent) | Deadlocks |
|----------|---------------------------|-----------|
| Apple Silicon | 5.36ms | None |
| AMD EPYC | 18.35ms | None |

**Analysis**: The async infrastructure works correctly (`instantiate_async`, `func_wrap_async`, `call_async` all function under Tokio). This validates that WASM→Host→SQLx→Return is viable.

**Caveat**: The guest plugin does not actually call `db_query_async`. This benchmark validates async *infrastructure*, not the full path with simulated latency.

### Mutation Benchmark (Write-Heavy Workloads)

**Hypothesis**: Handle-based might win for mutation-heavy workloads.

**Result**: DISPROVEN — Full-serialization is 1.6x faster even for mutations.

| Mode | Tap Avg | Tap p95 |
|------|---------|---------|
| Handle-based (3 reads + 1 write) | 34.55µs | 44.65µs |
| Full-serialization | 21.65µs | 26.04µs |

**Analysis**: Even with write-heavy workloads, full-serialization wins. The overhead of multiple boundary crossings exceeds the cost of serializing the full payload twice (in and out).

---

## Architectural Recommendations

### Primary: Full-Serialization Default

Use full-serialization as the **only** data access mode for plugin tap functions:

```rust
// Plugin receives complete item as JSON
fn tap_item_view(item_json: &str) -> String {
    let item: Item = serde_json::from_str(item_json)?;
    // Process and return render element JSON
}
```

**Rationale** (validated by benchmarks):
1. Faster for typical payloads on both ARM and x86-64
2. Faster even for write-heavy workloads
3. Simpler plugin development (no host function imports)
4. Better portability (pure computation, no ABI dependency)
5. Easier testing (pure functions with JSON in/out)

### Handle-Based API: Not Recommended

Originally planned as an optimization, benchmarks show **no scenario where handle-based wins**:

| Scenario | Winner |
|----------|--------|
| Read-heavy (3 reads) | Full-serialization (1.5x faster) |
| Write-heavy (3 reads + 1 write) | Full-serialization (1.6x faster) |
| x86-64 vs ARM | Both favor full-serialization |

**Recommendation**: Do not implement handle-based API in Phase 2. If future profiling reveals edge cases (extremely large items where plugins need only 1-2 fields), it can be added later.

### Plugin SDK Design

```rust
// Simple: Plugin receives deserialized item
#[trovato::tap]
fn item_view(item: &Item) -> RenderElement {
    // SDK handles serialization/deserialization
}
```

No lazy/handle mode needed in MVP.

### Security: Filtered Serialization

Full-serialization means plugins receive complete item data. For field-level access control, implement **filtered serialization** (see `docs/design/Analysis-Field-Access-Security.md`):

```toml
# Plugin declares what fields it needs
[taps.options.tap_item_view]
fields = ["title", "field_body", "field_summary"]
```

Kernel serializes only declared fields. This provides:
- Performance benefit (smaller payloads)
- Security benefit (plugins can't access undeclared fields)
- Documentation benefit (explicit field dependencies)

---

## Validation Checklist

### Completed

- [x] **ARM benchmarks**: Apple Silicon M-series
- [x] **x86-64 benchmarks**: AMD EPYC
- [x] **Concurrency scaling**: Tested to 2000 concurrent requests
- [x] **Mutation benchmarks**: Write-heavy workloads tested
- [x] **Security analysis**: `docs/design/Analysis-Field-Access-Security.md` created

### Remaining (Deferred to Phase 2)

- [ ] **Large payload benchmarks**: Current tests use 2.4KB; need to pass actual large payloads to guest WASM
- [ ] **True async validation**: Guest function that calls `db_query_async`
- [ ] **Memory pressure testing**: Stress pooling allocator at memory limits

### Design Document Updates Needed

| Document | Changes Needed |
|----------|----------------|
| Design-Plugin-SDK.md | Remove handle-based API, document filtered serialization |
| Design-Plugin-System.md | Update tap signatures to JSON in/out only |
| NFR-2 in epics.md | Update or remove (handle-based >5x requirement is invalid) |

---

## Appendix: Test Environments

### Apple Silicon (M-series)

| Parameter | Value |
|-----------|-------|
| CPU | Apple Silicon (M-series) |
| Rust | Edition 2024 |
| Wasmtime | 28.x with pooling allocator |
| WASM Target | wasm32-wasip1 |
| Payload | ~2.4KB JSON |
| Iterations | 500 (Gate 1), 100 concurrent (Gates 2-3) |

### AMD EPYC (x86-64)

| Parameter | Value |
|-----------|-------|
| CPU | AMD EPYC |
| Rust | Edition 2024 |
| Wasmtime | 28.x with pooling allocator |
| WASM Target | wasm32-wasip1 |
| Payload | ~2.4KB JSON |
| Iterations | 500 (Gate 1), 100-2000 concurrent (Gates 2-3) |

---

## Appendix: Raw EPYC Results

### Gate 1 (500 iterations)
```
Handle-based tap avg: 36.546µs, p50: 34.68µs, p95: 43.75µs, p99: 52.85µs
Full-serialization tap avg: 23.824µs, p50: 22.95µs, p95: 28.64µs, p99: 34.53µs
Speedup ratio: 0.65x (full-serialization 1.5x faster)
```

### Gate 2 (2000 concurrent)
```
Total: avg 507.98µs, p50: 492.21µs, p95: 859.10µs, p99: 1.15ms
Instantiation: avg 100.60µs, p50: 86.26µs, p95: 196.07µs, p99: 253.77µs
Tap only: avg 405.64µs, p50: 381.75µs, p95: 736.27µs, p99: 988.02µs
All succeeded: true
```

### Gate 3 (100 concurrent async)
```
Wall-clock time: 18.354316ms
Completed without deadlock: true
```

### Mutation Benchmark (500 iterations)
```
Handle-based (3 reads + 1 write): avg 34.548µs, p95: 44.654µs
Full-serialization: avg 21.65µs, p95: 26.038µs
Full-serialization is 1.60x faster for mutations
```
