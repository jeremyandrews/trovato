//! Benchmark 1: Handle-based data access.
//!
//! Measures the cost of reading 3 fields + modifying 1 field + returning
//! a render element, using the handle-based API where each field access
//! is a separate host function call across the WASM boundary.
//!
//! Target: 500 calls in <250ms (0.5ms per call).

// TODO: Implement once guest plugin is compiled to WASM.
//
// Steps:
// 1. Compile guest plugin with handle-based exports
// 2. For each call:
//    a. Create Store with StubHostState
//    b. Load fixture item at handle 0
//    c. Call tap-item-view(0)
//    d. Guest calls get-title, get-field-string("field_body"),
//       get-field-string("field_summary") [3 reads]
//    e. Guest calls set-field-string("field_computed", ...) [1 write]
//    f. Guest returns RenderElement JSON
// 3. Collect per-call timings, compute stats
