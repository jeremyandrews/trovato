//! Benchmark 3: Async host functions (WASM -> Rust -> SQLx bridge).
//!
//! Validates that async host function calls from WASM guests work
//! correctly under the Tokio runtime without deadlocks. Each tap call
//! executes a (stubbed) database query via an async host function.
//!
//! Target: no deadlocks; latency ~1us overhead + query time.

// TODO: Implement once guest plugin is compiled to WASM.
//
// Steps:
// 1. Define async host functions using wasmtime's async support:
//    - db_query: async fn that simulates a database round-trip
//    - user_has_permission: async fn (fast lookup)
// 2. Enable async support on Engine config
// 3. Use linker.func_wrap_async() for host functions
// 4. Use linker.instantiate_async() for plugin instantiation
// 5. Run multiple tap calls, verifying:
//    a. No deadlocks (complete within timeout)
//    b. Correct return values from async host functions
//    c. Measure overhead of async boundary crossing
// 6. Run under load (50 concurrent async tap calls)
