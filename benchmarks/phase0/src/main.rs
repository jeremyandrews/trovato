//! Phase 0: WASM Architecture Validation
//!
//! Three critical benchmarks before writing any Kernel code:
//!
//! 1. Handle-based vs full-serialization data access (500 calls each)
//!    - Per call: read 3 fields + modify 1 field + return render element
//!    - Payload: 4KB JSON (15 fields, nested arrays, record references)
//!    - Gate: handle-based must be >5x faster than full-serialization
//!
//! 2. Store pooling under concurrency (100 parallel requests)
//!    - Per request: instantiate plugin -> call tap -> return
//!    - Gate: <10ms p95 per request end-to-end
//!
//! 3. Async host functions (WASM -> Rust -> SQLx bridge)
//!    - Each tap call executes a database query via host function
//!    - Gate: no deadlocks under Tokio runtime
//!
//! This is a standalone binary, not the full kernel. It uses wasmtime
//! directly with a pooling allocator and stub host functions.

mod bench_async;
mod bench_concurrency;
mod bench_handle;
mod bench_serialize;
mod fixture;
mod host;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use host::{BenchHost, HostConfig};

/// Path to the compiled guest plugin WASM (debug or release based on availability).
fn guest_wasm_path() -> &'static str {
    let release_path = "target/wasm32-wasip1/release/phase0_guest.wasm";
    let debug_path = "target/wasm32-wasip1/debug/phase0_guest.wasm";

    if std::path::Path::new(release_path).exists() {
        release_path
    } else {
        debug_path
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok();

    println!("=== Trovato Phase 0: WASM Architecture Validation ===\n");

    // Initialize benchmark host
    println!("Initializing benchmark host...");
    let config = HostConfig {
        max_instances: 1000,
        max_memory_pages: 1024,
        async_support: false, // Disable for now, enable for Story 1.5
    };
    let host = BenchHost::with_config(&config)?;
    println!("  ✓ Engine created with pooling allocator\n");

    // Load the guest plugin
    println!("Loading guest plugin...");
    let wasm_path = PathBuf::from(guest_wasm_path());
    if !wasm_path.exists() {
        println!("  ✗ Guest plugin not found at: {}", wasm_path.display());
        println!("  Build it with: cargo build --target wasm32-wasip1 -p phase0-guest");
        return Ok(());
    }

    let module = host.compile_from_file(&wasm_path)?;
    println!("  ✓ Loaded: {}", wasm_path.display());
    println!();

    // Verify both modes work before benchmarking
    println!("Verifying plugin functionality...");
    bench_handle::verify_handle_access(&host, &module)?;
    bench_serialize::verify_serialize_access(&host, &module)?;
    println!("  ✓ Both data access modes verified\n");

    // Run benchmarks
    const ITERATIONS: u32 = 500;

    println!("Running handle-based benchmark ({} iterations)...", ITERATIONS);
    let handle_results = bench_handle::run_handle_benchmark(&host, &module, ITERATIONS)?;
    println!("  {}", handle_results.total);
    println!("  {}", handle_results.tap_only);
    println!("  {}\n", handle_results.instantiation_only);

    println!("Running full-serialization benchmark ({} iterations)...", ITERATIONS);
    let serialize_results = bench_serialize::run_serialize_benchmark(&host, &module, ITERATIONS)?;
    println!("  {}", serialize_results.total);
    println!("  {}", serialize_results.tap_only);
    println!("  {}\n", serialize_results.instantiation_only);

    // Calculate speedup ratios
    let handle_tap_avg = handle_results.tap_only.per_call_avg.as_nanos() as f64;
    let serialize_tap_avg = serialize_results.tap_only.per_call_avg.as_nanos() as f64;
    let tap_speedup = serialize_tap_avg / handle_tap_avg;

    let handle_total_avg = handle_results.total.per_call_avg.as_nanos() as f64;
    let serialize_total_avg = serialize_results.total.per_call_avg.as_nanos() as f64;
    let total_speedup = serialize_total_avg / handle_total_avg;

    println!("=== Results Summary ===\n");
    println!("Handle-based (tap call only):");
    println!("  Average: {:?}", handle_results.tap_only.per_call_avg);
    println!("  p50: {:?}, p95: {:?}, p99: {:?}",
             handle_results.tap_only.p50, handle_results.tap_only.p95, handle_results.tap_only.p99);
    println!();
    println!("Full-serialization (tap call only):");
    println!("  Average: {:?}", serialize_results.tap_only.per_call_avg);
    println!("  p50: {:?}, p95: {:?}, p99: {:?}",
             serialize_results.tap_only.p50, serialize_results.tap_only.p95, serialize_results.tap_only.p99);
    println!();
    println!("Tap-only speedup ratio: {:.2}x", tap_speedup);
    println!("Total (including instantiation) speedup ratio: {:.2}x", total_speedup);
    println!();

    println!("Instantiation overhead:");
    println!("  Handle-based: {:?}", handle_results.instantiation_only.per_call_avg);
    println!("  Full-serialization: {:?}", serialize_results.instantiation_only.per_call_avg);
    println!();

    // Gate check - use tap-only comparison for the architectural decision
    if tap_speedup >= 5.0 {
        println!("✓ GATE PASSED: Handle-based tap is {:.1}x faster (threshold: 5x)", tap_speedup);
        println!("  Recommendation: Use handle-based as the default data access mode.");
    } else if tap_speedup >= 2.0 {
        println!("⚠ GATE MARGINAL: Handle-based tap is only {:.1}x faster (threshold: 5x)", tap_speedup);
        println!("  Recommendation: Consider hybrid approach or further optimization.");
    } else if tap_speedup >= 1.0 {
        println!("⚠ GATE MARGINAL: Handle-based tap is only {:.1}x faster (threshold: 5x)", tap_speedup);
        println!("  Note: Instantiation dominates; both modes are viable.");
        println!("  Recommendation: Choose based on ergonomics (handle-based for partial access).");
    } else {
        println!("✗ UNEXPECTED: Full-serialization is faster than handle-based ({:.1}x)", 1.0/tap_speedup);
        println!("  Recommendation: Investigate handle-based overhead.");
    }

    Ok(())
}

/// Benchmark result for reporting.
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: String,
    pub total_calls: u64,
    pub total_time: Duration,
    pub per_call_avg: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

impl BenchResult {
    /// Create a BenchResult from a sorted list of durations.
    pub fn from_durations(name: impl Into<String>, durations: &[Duration]) -> Self {
        let total_calls = durations.len() as u64;
        let total_time: Duration = durations.iter().sum();
        let per_call_avg = if total_calls > 0 {
            total_time / total_calls as u32
        } else {
            Duration::ZERO
        };

        let p50 = durations
            .get(durations.len() / 2)
            .copied()
            .unwrap_or_default();
        let p95 = durations
            .get((durations.len() as f64 * 0.95) as usize)
            .copied()
            .unwrap_or_default();
        let p99 = durations
            .get((durations.len() as f64 * 0.99) as usize)
            .copied()
            .unwrap_or_default();

        Self {
            name: name.into(),
            total_calls,
            total_time,
            per_call_avg,
            p50,
            p95,
            p99,
        }
    }
}

impl std::fmt::Display for BenchResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} calls in {:.2?} (avg {:.2?}, p50 {:.2?}, p95 {:.2?}, p99 {:.2?})",
            self.name,
            self.total_calls,
            self.total_time,
            self.per_call_avg,
            self.p50,
            self.p95,
            self.p99,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_result_from_durations() {
        let durations: Vec<Duration> = (1..=100).map(|i| Duration::from_micros(i)).collect();
        let result = BenchResult::from_durations("test", &durations);

        assert_eq!(result.name, "test");
        assert_eq!(result.total_calls, 100);
        // p50 is at index 50 (middle of 100 elements) = 51µs (1-indexed data)
        assert_eq!(result.p50, Duration::from_micros(51));
        // p95 is at index 95 = 96µs
        assert_eq!(result.p95, Duration::from_micros(96));
        // p99 is at index 99 = 100µs
        assert_eq!(result.p99, Duration::from_micros(100));
    }
}
