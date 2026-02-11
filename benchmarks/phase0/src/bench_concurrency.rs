//! Benchmark 2: Store pooling under concurrency.
//!
//! Fires 100 parallel requests, each instantiating a plugin Store,
//! calling a tap, and returning. Validates that the pooling allocator
//! handles concurrent instantiation efficiently.
//!
//! Target: <10ms p95 per request end-to-end.

// TODO: Implement once guest plugin is compiled to WASM.
//
// Steps:
// 1. Initialize Engine with PoolingAllocationConfig
// 2. Pre-compile plugin module (Module is Send+Sync)
// 3. Spawn 100 tokio tasks, each:
//    a. Create new Store (from pool)
//    b. Instantiate plugin
//    c. Load fixture item
//    d. Call tap-item-view
//    e. Drop Store (returned to pool)
//    f. Record instantiation time + execution time
// 4. Collect all timings, compute p50/p95/p99 + throughput
