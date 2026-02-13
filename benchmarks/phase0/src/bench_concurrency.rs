//! Benchmark 2: Store pooling under concurrency.
//!
//! Fires 100 parallel requests, each instantiating a plugin Store,
//! calling a tap, and returning. Validates that the pooling allocator
//! handles concurrent instantiation efficiently.
//!
//! Target: <10ms p95 per request end-to-end.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::task::JoinSet;
use wasmtime::Module;

use crate::BenchResult;
use crate::fixture::synthetic_item;
use crate::host::{BenchHost, StubHostState};

/// Results from the concurrency benchmark.
pub struct ConcurrencyBenchmarkResults {
    /// Total request time (instantiation + tap call).
    pub total: BenchResult,
    /// Instantiation time only.
    pub instantiation_only: BenchResult,
    /// Tap call time only.
    pub tap_only: BenchResult,
    /// Number of concurrent requests.
    pub concurrency: u32,
    /// Whether all requests completed successfully.
    pub all_succeeded: bool,
}

/// Run the concurrency benchmark.
///
/// Spawns `concurrency` parallel tasks, each:
/// 1. Creating a new Store from the pool
/// 2. Instantiating the plugin
/// 3. Loading fixture data
/// 4. Calling tap_item_view
/// 5. Returning the result
///
/// Reports p50/p95/p99 latencies and validates the <10ms p95 gate.
pub async fn run_concurrency_benchmark(
    host: Arc<BenchHost>,
    module: Arc<Module>,
    concurrency: u32,
) -> Result<ConcurrencyBenchmarkResults> {
    let mut join_set: JoinSet<Result<(Duration, Duration, Duration)>> = JoinSet::new();

    // Spawn all tasks concurrently
    for _ in 0..concurrency {
        let host = Arc::clone(&host);
        let module = Arc::clone(&module);

        join_set.spawn(async move {
            let total_start = Instant::now();

            // Create fresh store with fixture data
            let mut state = StubHostState::new();
            state.load_item(0, synthetic_item());
            let mut store = host.create_store_with_state(state);

            // Instantiate the plugin
            let instance = host
                .linker
                .instantiate(&mut store, &module)
                .context("failed to instantiate plugin")?;

            let instantiation_elapsed = total_start.elapsed();

            // Get the tap function
            let tap_item_view: wasmtime::TypedFunc<i32, i64> = instance
                .get_typed_func(&mut store, "tap_item_view")
                .context("failed to get tap_item_view")?;

            // Time just the tap call
            let tap_start = Instant::now();
            let result = tap_item_view.call(&mut store, 0)?;
            let tap_elapsed = tap_start.elapsed();

            let total_elapsed = total_start.elapsed();

            // Verify we got a result
            let len = (result & 0xFFFFFFFF) as i32;
            anyhow::ensure!(len > 0, "tap_item_view should return non-empty JSON");

            Ok((total_elapsed, instantiation_elapsed, tap_elapsed))
        });
    }

    // Collect results
    let mut total_durations = Vec::with_capacity(concurrency as usize);
    let mut instantiation_durations = Vec::with_capacity(concurrency as usize);
    let mut tap_durations = Vec::with_capacity(concurrency as usize);
    let mut all_succeeded = true;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok((total, instantiation, tap))) => {
                total_durations.push(total);
                instantiation_durations.push(instantiation);
                tap_durations.push(tap);
            }
            Ok(Err(e)) => {
                tracing::error!("Task failed: {}", e);
                all_succeeded = false;
            }
            Err(e) => {
                tracing::error!("Task panicked: {}", e);
                all_succeeded = false;
            }
        }
    }

    // Sort for percentile calculation
    total_durations.sort();
    instantiation_durations.sort();
    tap_durations.sort();

    Ok(ConcurrencyBenchmarkResults {
        total: BenchResult::from_durations("concurrency (total)", &total_durations),
        instantiation_only: BenchResult::from_durations(
            "concurrency (instantiation)",
            &instantiation_durations,
        ),
        tap_only: BenchResult::from_durations("concurrency (tap only)", &tap_durations),
        concurrency,
        all_succeeded,
    })
}

/// Verify that concurrent execution works at all.
pub async fn verify_concurrency(host: Arc<BenchHost>, module: Arc<Module>) -> Result<()> {
    // Run a small concurrency test
    let results = run_concurrency_benchmark(host, module, 10).await?;

    anyhow::ensure!(
        results.all_succeeded,
        "Not all concurrent requests succeeded"
    );

    println!(
        "  Concurrency verification: {} requests completed",
        results.total.total_calls
    );
    println!("  Average total time: {:?}", results.total.per_call_avg);

    Ok(())
}
