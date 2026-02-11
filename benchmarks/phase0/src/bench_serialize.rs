//! Benchmark 1b: Full-serialization data access.
//!
//! Measures the cost of passing the entire 4KB item JSON across the WASM
//! boundary, parsing it in the guest, modifying a field, and returning
//! the modified JSON.
//!
//! Target: p95 <1ms for 4KB payloads.
//! Comparison: must be >5x slower than handle-based for handle-based
//! to become the default.

// TODO: Implement once guest plugin is compiled to WASM.
//
// Steps:
// 1. Compile guest plugin with full-serialization exports
// 2. For each call:
//    a. Create Store with StubHostState
//    b. Serialize fixture item to JSON string (~4KB)
//    c. Call tap-item-view-full(item_json)
//    d. Guest parses JSON, reads 3 fields, modifies 1, serializes back
//    e. Guest returns RenderElement JSON
// 3. Collect per-call timings, compute stats
// 4. Compare against handle-based results
