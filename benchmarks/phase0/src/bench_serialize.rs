//! Benchmark 1b: Full-serialization data access.
//!
//! Measures the cost of passing the entire 4KB item JSON across the WASM
//! boundary, parsing it in the guest, modifying a field, and returning
//! the modified JSON.
//!
//! Target: p95 <1ms for 4KB payloads.
//! Comparison: must be >5x slower than handle-based for handle-based
//! to become the default.

use std::time::Instant;

use anyhow::{Context, Result};
use wasmtime::{Module, TypedFunc};

use crate::fixture::synthetic_item;
use crate::host::BenchHost;
use crate::BenchResult;

/// Benchmark results including separate instantiation and tap call timing.
pub struct SerializeBenchmarkResults {
    pub total: BenchResult,
    pub tap_only: BenchResult,
    pub instantiation_only: BenchResult,
}

/// Run the full-serialization benchmark.
///
/// Executes `tap_item_view_full(json_ptr, json_len)` N times, measuring:
/// 1. Total time (instantiation + memory setup + tap call)
/// 2. Tap call time only
/// 3. Instantiation + memory setup time
pub fn run_serialize_benchmark(host: &BenchHost, module: &Module, iterations: u32) -> Result<SerializeBenchmarkResults> {
    let mut total_durations = Vec::with_capacity(iterations as usize);
    let mut tap_durations = Vec::with_capacity(iterations as usize);
    let mut instantiation_durations = Vec::with_capacity(iterations as usize);

    // Pre-serialize the fixture item
    let item_json = serde_json::to_string(&synthetic_item())?;
    let json_bytes = item_json.as_bytes();

    for _ in 0..iterations {
        let total_start = Instant::now();

        // Create fresh store
        let mut store = host.create_store();

        // Instantiate the plugin
        let instance = host
            .linker
            .instantiate(&mut store, module)
            .context("failed to instantiate plugin")?;

        // Get the alloc function to allocate memory for the JSON
        let alloc_fn: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, "alloc")
            .context("failed to get alloc export")?;

        // Allocate memory for the input JSON
        let json_ptr = alloc_fn.call(&mut store, json_bytes.len() as i32)?;

        // Write the JSON to WASM memory
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("failed to get memory")?;
        memory.data_mut(&mut store)[json_ptr as usize..json_ptr as usize + json_bytes.len()]
            .copy_from_slice(json_bytes);

        // Get the tap_item_view_full function
        let tap_item_view_full: TypedFunc<(i32, i32), i64> = instance
            .get_typed_func(&mut store, "tap_item_view_full")
            .context("failed to get tap_item_view_full export")?;

        let instantiation_elapsed = total_start.elapsed();
        instantiation_durations.push(instantiation_elapsed);

        // Time just the tap call (including JSON parsing in guest)
        let tap_start = Instant::now();
        let result = tap_item_view_full.call(&mut store, (json_ptr, json_bytes.len() as i32))?;
        let tap_elapsed = tap_start.elapsed();
        tap_durations.push(tap_elapsed);

        let total_elapsed = total_start.elapsed();
        total_durations.push(total_elapsed);

        // Verify we got a result
        let _ptr = (result >> 32) as i32;
        let len = (result & 0xFFFFFFFF) as i32;
        assert!(len > 0, "tap_item_view_full should return non-empty JSON");
    }

    // Sort for percentile calculation
    total_durations.sort();
    tap_durations.sort();
    instantiation_durations.sort();

    Ok(SerializeBenchmarkResults {
        total: BenchResult::from_durations("full-serialization (total)", &total_durations),
        tap_only: BenchResult::from_durations("full-serialization (tap only)", &tap_durations),
        instantiation_only: BenchResult::from_durations("full-serialization (instantiation)", &instantiation_durations),
    })
}

/// Run a quick verification that full-serialization access works.
pub fn verify_serialize_access(host: &BenchHost, module: &Module) -> Result<()> {
    let item_json = serde_json::to_string(&synthetic_item())?;
    let json_bytes = item_json.as_bytes();

    let mut store = host.create_store();

    let instance = host
        .linker
        .instantiate(&mut store, module)
        .context("failed to instantiate plugin for verification")?;

    // Allocate and write JSON
    let alloc_fn: TypedFunc<i32, i32> = instance
        .get_typed_func(&mut store, "alloc")
        .context("failed to get alloc")?;

    let json_ptr = alloc_fn.call(&mut store, json_bytes.len() as i32)?;

    let memory = instance
        .get_memory(&mut store, "memory")
        .context("failed to get memory")?;
    memory.data_mut(&mut store)[json_ptr as usize..json_ptr as usize + json_bytes.len()]
        .copy_from_slice(json_bytes);

    // Call tap_item_view_full
    let tap_item_view_full: TypedFunc<(i32, i32), i64> = instance
        .get_typed_func(&mut store, "tap_item_view_full")
        .context("failed to get tap_item_view_full")?;

    let result = tap_item_view_full.call(&mut store, (json_ptr, json_bytes.len() as i32))?;
    let ptr = (result >> 32) as i32;
    let len = (result & 0xFFFFFFFF) as i32;

    // Read the result JSON from WASM memory
    let data = memory.data(&store);
    let result_bytes = &data[ptr as usize..(ptr + len) as usize];
    let result_json = std::str::from_utf8(result_bytes).context("invalid UTF-8 in result")?;

    println!("  Full-serialization result preview: {}...", &result_json[..result_json.len().min(80)]);
    println!("  Input JSON size: {} bytes", json_bytes.len());

    Ok(())
}
