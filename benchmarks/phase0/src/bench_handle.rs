//! Benchmark 1: Handle-based data access.
//!
//! Measures the cost of reading 3 fields + modifying 1 field + returning
//! a render element, using the handle-based API where each field access
//! is a separate host function call across the WASM boundary.
//!
//! Target: 500 calls in <250ms (0.5ms per call).

use std::time::Instant;

use anyhow::{Context, Result};
use wasmtime::{Module, TypedFunc};

use crate::fixture::synthetic_item;
use crate::host::{BenchHost, StubHostState};
use crate::BenchResult;

/// Benchmark results including separate instantiation and tap call timing.
pub struct HandleBenchmarkResults {
    pub total: BenchResult,
    pub tap_only: BenchResult,
    pub instantiation_only: BenchResult,
}

/// Run the handle-based benchmark.
///
/// Executes `tap_item_view(handle)` N times, measuring:
/// 1. Total time (instantiation + tap call)
/// 2. Tap call time only
/// 3. Instantiation time only
pub fn run_handle_benchmark(host: &BenchHost, module: &Module, iterations: u32) -> Result<HandleBenchmarkResults> {
    let mut total_durations = Vec::with_capacity(iterations as usize);
    let mut tap_durations = Vec::with_capacity(iterations as usize);
    let mut instantiation_durations = Vec::with_capacity(iterations as usize);

    for _ in 0..iterations {
        let total_start = Instant::now();

        // Create fresh store with fixture data
        let mut state = StubHostState::new();
        state.load_item(0, synthetic_item());
        let mut store = host.create_store_with_state(state);

        // Instantiate the plugin
        let instance = host
            .linker
            .instantiate(&mut store, module)
            .context("failed to instantiate plugin")?;

        // Get the tap_item_view function
        let tap_item_view: TypedFunc<i32, i64> = instance
            .get_typed_func(&mut store, "tap_item_view")
            .context("failed to get tap_item_view export")?;

        let instantiation_elapsed = total_start.elapsed();
        instantiation_durations.push(instantiation_elapsed);

        // Time just the tap call
        let tap_start = Instant::now();
        let result = tap_item_view.call(&mut store, 0)?;
        let tap_elapsed = tap_start.elapsed();
        tap_durations.push(tap_elapsed);

        let total_elapsed = total_start.elapsed();
        total_durations.push(total_elapsed);

        // Verify we got a result (ptr << 32 | len)
        let _ptr = (result >> 32) as i32;
        let len = (result & 0xFFFFFFFF) as i32;
        assert!(len > 0, "tap_item_view should return non-empty JSON");
    }

    // Sort for percentile calculation
    total_durations.sort();
    tap_durations.sort();
    instantiation_durations.sort();

    Ok(HandleBenchmarkResults {
        total: BenchResult::from_durations("handle-based (total)", &total_durations),
        tap_only: BenchResult::from_durations("handle-based (tap only)", &tap_durations),
        instantiation_only: BenchResult::from_durations("handle-based (instantiation)", &instantiation_durations),
    })
}

/// Run a quick verification that handle-based access works.
pub fn verify_handle_access(host: &BenchHost, module: &Module) -> Result<()> {
    let mut state = StubHostState::new();
    state.load_item(0, synthetic_item());
    let mut store = host.create_store_with_state(state);

    let instance = host
        .linker
        .instantiate(&mut store, module)
        .context("failed to instantiate plugin for verification")?;

    let tap_item_view: TypedFunc<i32, i64> = instance
        .get_typed_func(&mut store, "tap_item_view")
        .context("failed to get tap_item_view")?;

    let result = tap_item_view.call(&mut store, 0)?;
    let ptr = (result >> 32) as i32;
    let len = (result & 0xFFFFFFFF) as i32;

    // Read the result JSON from WASM memory
    let memory = instance
        .get_memory(&mut store, "memory")
        .context("failed to get memory export")?;

    let data = memory.data(&store);
    let json_bytes = &data[ptr as usize..(ptr + len) as usize];
    let json = std::str::from_utf8(json_bytes).context("invalid UTF-8 in result")?;

    println!("  Handle-based result preview: {}...", &json[..json.len().min(80)]);

    // Verify the computed field was set
    let computed = store.data().get_field_string(0, "field_computed");
    assert!(
        computed.is_some(),
        "field_computed should have been set by the plugin"
    );
    println!("  Computed field: {:?}", computed);

    Ok(())
}
