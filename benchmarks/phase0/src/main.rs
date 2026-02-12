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

use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use host::{BenchHost, HostConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok();

    println!("=== Trovato Phase 0: WASM Architecture Validation ===\n");

    // Story 1.1: Initialize Wasmtime Engine with pooling allocator
    println!("Initializing benchmark host environment...");
    let start = Instant::now();

    let config = HostConfig {
        max_instances: 1000,
        max_memory_pages: 1024, // 64MB max per instance
        async_support: true,
    };

    let host = BenchHost::with_config(&config)?;
    let init_time = start.elapsed();

    println!("  ✓ Engine created with pooling allocator");
    println!("  ✓ Linker configured with host functions (log, variables)");
    println!("  ✓ Initialization time: {init_time:?}");
    println!();

    // Verify fixture generation
    println!("Verifying test fixtures...");
    let item = fixture::synthetic_item();
    let item_size = fixture::synthetic_item_size();
    println!("  ✓ Synthetic item payload: {item_size} bytes (~4KB target)");
    println!(
        "  ✓ Item type: {}",
        item.get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    );
    println!(
        "  ✓ Field count: {}",
        item.get("fields")
            .and_then(|f| f.as_object())
            .map(|f| f.len())
            .unwrap_or(0)
    );
    println!();

    // Verify store creation
    println!("Verifying store creation...");
    let store_start = Instant::now();
    let _store = host.create_store();
    let store_time = store_start.elapsed();
    println!("  ✓ Store creation time: {store_time:?}");

    // Create multiple stores to verify pooling
    let mut store_times = Vec::with_capacity(100);
    for _ in 0..100 {
        let s = Instant::now();
        let _store = host.create_store();
        store_times.push(s.elapsed());
    }
    store_times.sort();
    let p50 = store_times[49];
    let p95 = store_times[94];
    let p99 = store_times[98];
    println!("  ✓ Store creation (100 iterations): p50={p50:?}, p95={p95:?}, p99={p99:?}");
    println!();

    // Summary
    println!("=== Story 1.1 Complete ===");
    println!();
    println!("Benchmark host binary initialized successfully.");
    println!("Next steps (Story 1.2):");
    println!("  1. Create minimal test guest plugin (wasm32-wasip1 target)");
    println!("  2. Implement tap-item-view export in guest");
    println!("  3. Verify plugin loads and executes in benchmark host");
    println!();
    println!("Remaining Phase 0 stories:");
    println!("  1.3: Implement handle-based host functions (item-api)");
    println!("  1.4: Run handle vs serialize benchmark (500 calls)");
    println!("  1.5: Run concurrency benchmark (100 parallel)");
    println!("  1.6: Run async host function validation");

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
