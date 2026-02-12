# Phase 0: WASM Architecture Validation - Complete

**Date**: 2026-02-12
**Status**: Gates 2/3 Passed, Gate 1 Failed
**Recommendation**: Proceed with full-serialization as default data access mode

## Executive Summary

Phase 0 validated the core WASM plugin architecture through three critical benchmarks. The infrastructure is sound—concurrent instantiation and async host functions work excellently under Tokio. However, the handle-based data access hypothesis was disproven: full-serialization is faster for typical payloads.

**Decision**: Proceed to Phase 1 using full-serialization as the default plugin data access mode, with handle-based available as an optimization for specific use cases.

## Gate Status

| Gate | Requirement | Result | Status |
|------|-------------|--------|--------|
| 1 | Handle-based >5x faster | 0.86x (full-serialization faster) | **FAILED** |
| 2 | Concurrency p95 <10ms | 1.09ms | **PASSED** |
| 3 | Async without deadlock | No deadlocks, 5.4ms | **PASSED** |

**Overall**: PROCEED WITH MODIFICATIONS — The WASM architecture is validated. The data access mode changes from handle-based to full-serialization, but this is a design simplification, not a blocker.

---

## Benchmark Results

### Gate 1: Handle-Based vs Full-Serialization

**Hypothesis**: Handle-based access (one host call per field) would be >5x faster than full-serialization (single JSON blob across boundary).

**Result**: FAILED — Full-serialization is 1.2x faster.

| Metric | Handle-Based | Full-Serialization |
|--------|--------------|-------------------|
| Tap avg | 27.16µs | 23.39µs |
| Tap p50 | 26.00µs | 22.50µs |
| Tap p95 | 33.75µs | 29.29µs |
| Tap p99 | 43.00µs | 36.92µs |
| Instantiation avg | 48.14µs | 30.29µs |

**Analysis**: The handle-based approach requires 4 WASM↔host boundary crossings for the test workload (read 3 fields + write 1 field). Each crossing involves context switch overhead, parameter marshaling, memory buffer management, and string copying. Full-serialization does a single crossing with ~2.4KB JSON—modern JSON parsing is fast enough that reduced boundary crossings win.

**Surprising finding**: Handle-based instantiation is also slower (48µs vs 30µs), possibly due to additional host function registrations or linker complexity.

### Gate 2: Store Pooling Concurrency

**Requirement**: 100 parallel requests with p95 <10ms per request.

**Result**: PASSED — p95 = 1.09ms (10x better than threshold)

| Metric | Value |
|--------|-------|
| Requests | 100 concurrent |
| Total p95 | 1.09ms |
| Total p99 | 1.38ms |
| Instantiation p95 | 361µs |
| Tap p95 | 862µs |

**Analysis**: The wasmtime pooling allocator handles concurrent instantiation efficiently. This validates the per-request instantiation model—we don't need complex instance pooling or reuse strategies.

### Gate 3: Async Host Functions

**Requirement**: No deadlocks under Tokio runtime with async host functions.

**Result**: PASSED — 100 concurrent async operations completed in 5.4ms wall-clock.

| Metric | Value |
|--------|-------|
| Requests | 100 concurrent |
| Wall-clock time | 5.36ms |
| Avg per request | 688µs |
| Deadlocks | None |

**Analysis**: The async infrastructure works correctly (`instantiate_async`, `func_wrap_async`, `call_async` all function under Tokio). This validates that WASM→Host→SQLx→Return is viable.

**Important caveat**: The guest plugin's `tap_item_view` does not actually call `db_query_async`. This benchmark validates async *infrastructure*, not the full path with simulated latency. The 688µs average (not 10ms+) confirms this. See "Known Limitations" appendix.

---

## Architectural Recommendations

### Primary: Full-Serialization Default

Use full-serialization as the default data access mode for plugin tap functions:

```rust
// Plugin receives complete item as JSON
fn tap_item_view(item_json: &str) -> String {
    let item: Item = serde_json::from_str(item_json)?;
    // Process and return render element JSON
}
```

**Rationale**:
1. Faster for typical payloads (1-10KB)
2. Simpler plugin development (no host function imports)
3. Better portability (pure computation, no ABI dependency)
4. Easier testing (pure functions with JSON in/out)

### Secondary: Handle-Based for Optimization

Retain handle-based API for specific scenarios:
- **Large nested data**: Items with arrays/objects plugins rarely need
- **Lazy loading**: Only 1-2 fields needed from large records
- **Streaming**: Processing item lists where only summaries are needed
- **Write-heavy**: Plugins primarily modify fields without reading

### Plugin SDK Design

```rust
// Default: Plugin receives deserialized item
#[trovato::tap]
fn item_view(item: &Item) -> RenderElement {
    // SDK handles serialization/deserialization
}

// Opt-in: Plugin receives handle for lazy access
#[trovato::tap(lazy)]
fn item_view(item: ItemHandle) -> RenderElement {
    let title = item.title()?;  // Host call only if needed
}
```

The SDK should default to full-serialization, provide opt-in handle API, and hide the choice from most plugin authors.

### Future: Hybrid Approach (Phase 2+)

Consider for Phase 2: serialize common fields eagerly, provide handles for the rest, let plugins declare field dependencies. Defer until simpler model is validated.

---

## Fallback Options

If WASM proves problematic, consider these alternatives:

| Option | Benefits | Trade-offs |
|--------|----------|------------|
| [Extism](https://extism.org/) | Simpler API, cross-language SDKs | Less control over memory/optimization |
| Scripting (Lua/Rhai/QuickJS) | Faster iteration, no compilation | Less isolation, larger attack surface |
| Native plugins (dylib) | Maximum performance, full Rust ecosystem | No sandboxing, security risk, ABI issues |

**Recommendation**: Stay with wasmtime—benchmarks show adequate performance, and security/isolation benefits are significant.

**Trigger thresholds** for reconsidering: p95 >50ms, memory >500MB, deadlocks under async, or persistent developer friction with WASM toolchain.

---

## Next Steps & Follow-up Work

### Immediate Actions
1. ~~Update design documents to reflect full-serialization default~~ → Deferred to Phase 2 start
2. Proceed to Phase 1 (Skeleton) with validated architecture
3. Implement Plugin SDK with full-serialization as primary mode (Phase 2)

### Required Before Plugin SDK Design
- [ ] **Security review**: Evaluate field-level access control implications of full-serialization vs handle-based
- [ ] **Document original handle-based rationale**: Why did design docs assume handle-based would be faster?

### Required Before Phase 1 Completion
- [ ] **x86-64 validation**: Run benchmarks on production hardware (AMD EPYC or Intel Xeon)
- [ ] **Large payload benchmarks**: Test with 10KB, 50KB, 100KB payloads
- [ ] **Write-path benchmarks**: Test mutation-heavy plugin workloads (modify 10+ fields)

### Design Document Updates

| Document | Changes Needed |
|----------|----------------|
| Design-Plugin-SDK.md | Default to full-serialization, document handle-based as optimization |
| Design-Plugin-System.md | Update tap signatures to JSON in/out, add payload size guidance |
| Design-Query-Engine.md | Consider field selection optimization |

### Technical Debt (Recommended)
- [ ] **True async validation**: Guest function that calls `db_query_async` for full path testing
- [ ] **Memory pressure testing**: Stress pooling allocator at memory limits
- [ ] **High concurrency testing**: 1000+ concurrent tasks to find actual limits

---

## Appendix: Test Environment & Methodology

| Parameter | Value |
|-----------|-------|
| CPU | Apple Silicon (M-series) |
| Rust | Edition 2024 |
| Wasmtime | 28.x with pooling allocator |
| WASM Target | wasm32-wasip1 |
| Tokio | Multi-threaded runtime |
| Payload | ~2.4KB JSON (15 fields, nested arrays, record references) |
| Iterations | 500 (Gate 1), 100 concurrent (Gates 2-3) |
| Warmup | None (cold start each iteration) |
| Statistics | Durations sorted, percentiles by index |

**Caveats**:
- **ARM-only**: Production typically uses x86-64. Validate on production hardware before Phase 1 completion.
- **Single payload size**: Real CMS items may be 10-100KB. Validate scaling assumptions with larger payloads.

---

## Appendix: Known Limitations & Security

### Gate 3 Clarification

The async benchmark validates wasmtime's async infrastructure works under Tokio, but the guest plugin does not call the `db_query_async` host function. The 688µs average (not 10ms+) confirms this. **Recommendation**: Add guest function exercising async host calls for complete validation.

### Untested Scenarios

1. **Write-heavy workloads**: Mutation-heavy plugins serialize entire item twice (in and out)—handle-based might win
2. **Memory pressure**: Behavior when instances approach 64MB limit is unknown
3. **High concurrency**: 100 tasks on 8+ cores tests parallelism; 1000+ task behavior is unknown

### Security Consideration

Full-serialization means plugins receive complete item data. Handle-based access *could* enforce field-level access control (plugin only sees requested fields). **This trade-off must be evaluated during Plugin SDK design.**
