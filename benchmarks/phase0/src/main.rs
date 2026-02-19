// Benchmark helpers and payload types are conditionally used depending on CLI flags.
#![allow(dead_code)]
//! Phase 0: WASM Architecture Validation
//!
//! Extended benchmark suite with configurable parameters for:
//! - Payload sizes (small/medium/large/xlarge)
//! - Concurrency levels (100/500/1000+)
//! - Benchmark types (handle, serialize, concurrency, async, mutation)
//!
//! Usage:
//!   cargo run --release -p trovato-phase0 -- --help
//!   cargo run --release -p trovato-phase0 -- --benchmark all
//!   cargo run --release -p trovato-phase0 -- --benchmark serialize --payload large
//!   cargo run --release -p trovato-phase0 -- --benchmark concurrency --concurrency 1000

mod bench_async;
mod bench_concurrency;
mod bench_handle;
mod bench_serialize;
mod fixture;
mod host;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use bench_async::AsyncBenchHost;
use fixture::PayloadSize;
use host::{BenchHost, HostConfig};

/// CLI configuration parsed from command line arguments.
struct BenchConfig {
    benchmark: BenchmarkType,
    payload_size: PayloadSize,
    iterations: u32,
    concurrency: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BenchmarkType {
    All,
    Handle,
    Serialize,
    Concurrency,
    Async,
    PayloadScaling, // Tests all payload sizes
    Mutation,       // Write-heavy workload
}

impl std::str::FromStr for BenchmarkType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "all" => Ok(BenchmarkType::All),
            "handle" => Ok(BenchmarkType::Handle),
            "serialize" | "serialization" => Ok(BenchmarkType::Serialize),
            "concurrency" | "concurrent" => Ok(BenchmarkType::Concurrency),
            "async" => Ok(BenchmarkType::Async),
            "payload" | "payload-scaling" | "payloads" => Ok(BenchmarkType::PayloadScaling),
            "mutation" | "mutations" | "write" => Ok(BenchmarkType::Mutation),
            _ => Err(format!(
                "Unknown benchmark: {s}. Use: all, handle, serialize, concurrency, async, payload, mutation"
            )),
        }
    }
}

fn parse_args() -> BenchConfig {
    let args: Vec<String> = std::env::args().collect();

    let mut config = BenchConfig {
        benchmark: BenchmarkType::All,
        payload_size: PayloadSize::Small,
        iterations: 500,
        concurrency: 100,
    };

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--benchmark" | "-b" => {
                i += 1;
                if i < args.len() {
                    config.benchmark = args[i].parse().unwrap_or_else(|e| {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    });
                }
            }
            "--payload" | "-p" => {
                i += 1;
                if i < args.len() {
                    config.payload_size = args[i].parse().unwrap_or_else(|e| {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    });
                }
            }
            "--iterations" | "-i" => {
                i += 1;
                if i < args.len() {
                    config.iterations = args[i].parse().unwrap_or_else(|_| {
                        eprintln!("Error: Invalid iteration count");
                        std::process::exit(1);
                    });
                }
            }
            "--concurrency" | "-c" => {
                i += 1;
                if i < args.len() {
                    config.concurrency = args[i].parse().unwrap_or_else(|_| {
                        eprintln!("Error: Invalid concurrency level");
                        std::process::exit(1);
                    });
                }
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_help();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    config
}

fn print_help() {
    println!("Trovato Phase 0: WASM Architecture Validation Benchmarks");
    println!();
    println!("USAGE:");
    println!("    cargo run --release -p trovato-phase0 -- [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -b, --benchmark <TYPE>     Benchmark to run [default: all]");
    println!("                               Types: all, handle, serialize, concurrency,");
    println!("                                      async, payload, mutation");
    println!("    -p, --payload <SIZE>       Payload size [default: small]");
    println!("                               Sizes: small (~2.4KB), medium (~10KB),");
    println!("                                      large (~50KB), xlarge (~100KB)");
    println!("    -i, --iterations <N>       Iterations for sequential benchmarks [default: 500]");
    println!("    -c, --concurrency <N>      Concurrent tasks [default: 100]");
    println!("    -h, --help                 Print this help message");
    println!();
    println!("EXAMPLES:");
    println!("    # Run all benchmarks with defaults");
    println!("    cargo run --release -p trovato-phase0");
    println!();
    println!("    # Test large payloads");
    println!("    cargo run --release -p trovato-phase0 -- --benchmark payload");
    println!();
    println!("    # High concurrency test");
    println!(
        "    cargo run --release -p trovato-phase0 -- --benchmark concurrency --concurrency 1000"
    );
    println!();
    println!("    # Mutation-heavy workload");
    println!("    cargo run --release -p trovato-phase0 -- --benchmark mutation --iterations 500");
    println!();
    println!("BUILDING GUEST WASM:");
    println!("    cargo build --target wasm32-wasip1 -p phase0-guest --release");
}

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
    let config = parse_args();

    // Initialize tracing
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok();

    println!("=== Trovato Phase 0: WASM Architecture Validation ===\n");

    // Show actual payload sizes
    println!("Payload sizes:");
    for size in [
        PayloadSize::Small,
        PayloadSize::Medium,
        PayloadSize::Large,
        PayloadSize::XLarge,
    ] {
        let actual = fixture::synthetic_item_size_for(size);
        println!("  {}: {} bytes", size.name(), actual);
    }
    println!();

    // Initialize benchmark host with higher instance limit for stress tests
    let max_instances = std::cmp::max(config.concurrency as u32 * 2, 2000);
    println!("Initializing benchmark host (max {max_instances} instances)...");
    let host_config = HostConfig {
        max_instances,
        max_memory_pages: 1024,
        async_support: false,
    };
    let host = BenchHost::with_config(&host_config)?;
    println!("  ✓ Engine created with pooling allocator\n");

    // Load the guest plugin
    println!("Loading guest plugin...");
    let wasm_path = PathBuf::from(guest_wasm_path());
    if !wasm_path.exists() {
        println!("  ✗ Guest plugin not found at: {}", wasm_path.display());
        println!("  Build it with: cargo build --target wasm32-wasip1 -p phase0-guest --release");
        return Ok(());
    }

    let module = host.compile_from_file(&wasm_path)?;
    println!("  ✓ Loaded: {}", wasm_path.display());
    println!();

    // Wrap in Arc for benchmarks that need concurrent access
    let host = Arc::new(host);
    let module = Arc::new(module);

    // Run selected benchmarks
    match config.benchmark {
        BenchmarkType::All => {
            run_gate_benchmarks(Arc::clone(&host), Arc::clone(&module), &config, &wasm_path)
                .await?;
        }
        BenchmarkType::Handle => {
            run_handle_benchmark(&host, &module, &config)?;
        }
        BenchmarkType::Serialize => {
            run_serialize_benchmark(&host, &module, &config)?;
        }
        BenchmarkType::Concurrency => {
            run_concurrency_benchmark(Arc::clone(&host), Arc::clone(&module), &config).await?;
        }
        BenchmarkType::Async => {
            run_async_benchmark(&wasm_path, &config).await?;
        }
        BenchmarkType::PayloadScaling => {
            run_payload_scaling_benchmark(&host, &module, &config)?;
        }
        BenchmarkType::Mutation => {
            run_mutation_benchmark(&host, &module, &config)?;
        }
    }

    Ok(())
}

/// Run all three gate benchmarks (original Phase 0 validation).
async fn run_gate_benchmarks(
    host: Arc<BenchHost>,
    module: Arc<wasmtime::Module>,
    config: &BenchConfig,
    wasm_path: &Path,
) -> Result<()> {
    // Verify both modes work before benchmarking
    println!("Verifying plugin functionality...");
    bench_handle::verify_handle_access(&host, &module)?;
    bench_serialize::verify_serialize_access(&host, &module)?;
    println!("  ✓ Both data access modes verified\n");

    // Gate 1: Handle vs Serialize
    println!("=== Gate 1: Handle-Based vs Full-Serialization ===\n");

    println!(
        "Running handle-based benchmark ({} iterations, {} payload)...",
        config.iterations,
        config.payload_size.name()
    );
    let handle_results = bench_handle::run_handle_benchmark(&host, &module, config.iterations)?;
    println!("  {}", handle_results.total);
    println!("  {}", handle_results.tap_only);
    println!();

    println!(
        "Running full-serialization benchmark ({} iterations, {} payload)...",
        config.iterations,
        config.payload_size.name()
    );
    let serialize_results =
        bench_serialize::run_serialize_benchmark(&host, &module, config.iterations)?;
    println!("  {}", serialize_results.total);
    println!("  {}", serialize_results.tap_only);
    println!();

    // Calculate and report speedup
    let handle_tap_avg = handle_results.tap_only.per_call_avg.as_nanos() as f64;
    let serialize_tap_avg = serialize_results.tap_only.per_call_avg.as_nanos() as f64;
    let tap_speedup = serialize_tap_avg / handle_tap_avg;

    println!("Results:");
    println!(
        "  Handle-based tap avg: {:?}",
        handle_results.tap_only.per_call_avg
    );
    println!(
        "  Full-serialization tap avg: {:?}",
        serialize_results.tap_only.per_call_avg
    );
    println!("  Speedup ratio: {tap_speedup:.2}x");
    println!();

    let gate1_passed = tap_speedup >= 5.0;
    if tap_speedup >= 5.0 {
        println!("✓ GATE 1 PASSED: Handle-based is {tap_speedup:.1}x faster");
    } else if tap_speedup >= 1.0 {
        println!("✗ GATE 1 FAILED: Handle-based is only {tap_speedup:.1}x faster (need 5x)");
    } else {
        println!(
            "✗ GATE 1 FAILED: Full-serialization is {:.1}x faster",
            1.0 / tap_speedup
        );
    }
    println!();

    // Gate 2: Concurrency
    println!("=== Gate 2: Store Pooling Concurrency ===\n");

    println!(
        "Running concurrency benchmark ({} parallel requests)...",
        config.concurrency
    );
    let concurrency_results = bench_concurrency::run_concurrency_benchmark(
        Arc::clone(&host),
        Arc::clone(&module),
        config.concurrency,
    )
    .await?;

    println!("  {}", concurrency_results.total);
    println!("  Total p95: {:?}", concurrency_results.total.p95);
    println!();

    let gate2_passed = concurrency_results.total.p95 < Duration::from_millis(10);
    if gate2_passed {
        println!(
            "✓ GATE 2 PASSED: p95 {:?} < 10ms",
            concurrency_results.total.p95
        );
    } else {
        println!(
            "✗ GATE 2 FAILED: p95 {:?} >= 10ms",
            concurrency_results.total.p95
        );
    }
    println!();

    // Gate 3: Async
    println!("=== Gate 3: Async Host Functions ===\n");

    let async_host = Arc::new(AsyncBenchHost::new()?);
    let async_module = Arc::new(async_host.compile_from_file(wasm_path)?);

    println!(
        "Running async benchmark ({} concurrent requests)...",
        config.concurrency
    );
    let async_results = bench_async::run_async_benchmark(
        Arc::clone(&async_host),
        Arc::clone(&async_module),
        config.concurrency,
    )
    .await?;

    println!("  Wall-clock time: {:?}", async_results.wall_clock_time);
    println!(
        "  Completed without deadlock: {}",
        async_results.completed_without_deadlock
    );
    println!();

    let gate3_passed = async_results.completed_without_deadlock
        && async_results.wall_clock_time < Duration::from_secs(2);
    if gate3_passed {
        println!(
            "✓ GATE 3 PASSED: No deadlocks, {:?} wall-clock",
            async_results.wall_clock_time
        );
    } else {
        println!("✗ GATE 3 FAILED");
    }
    println!();

    // Summary
    println!("=== Summary ===\n");
    println!(
        "Gate 1 (Handle >5x faster):    {}",
        if gate1_passed { "PASSED" } else { "FAILED" }
    );
    println!(
        "Gate 2 (Concurrency p95 <10ms): {}",
        if gate2_passed { "PASSED" } else { "FAILED" }
    );
    println!(
        "Gate 3 (Async no deadlock):     {}",
        if gate3_passed { "PASSED" } else { "FAILED" }
    );

    Ok(())
}

/// Run handle-based benchmark only.
fn run_handle_benchmark(
    host: &Arc<BenchHost>,
    module: &Arc<wasmtime::Module>,
    config: &BenchConfig,
) -> Result<()> {
    println!("=== Handle-Based Benchmark ===\n");
    println!("Payload: {}", config.payload_size.name());
    println!("Iterations: {}\n", config.iterations);

    bench_handle::verify_handle_access(host, module)?;
    let results = bench_handle::run_handle_benchmark(host, module, config.iterations)?;

    println!("Results:");
    println!("  {}", results.total);
    println!("  {}", results.tap_only);
    println!("  {}", results.instantiation_only);

    Ok(())
}

/// Run serialization benchmark only.
fn run_serialize_benchmark(
    host: &Arc<BenchHost>,
    module: &Arc<wasmtime::Module>,
    config: &BenchConfig,
) -> Result<()> {
    println!("=== Full-Serialization Benchmark ===\n");
    println!("Payload: {}", config.payload_size.name());
    println!("Iterations: {}\n", config.iterations);

    bench_serialize::verify_serialize_access(host, module)?;
    let results = bench_serialize::run_serialize_benchmark(host, module, config.iterations)?;

    println!("Results:");
    println!("  {}", results.total);
    println!("  {}", results.tap_only);
    println!("  {}", results.instantiation_only);

    Ok(())
}

/// Run concurrency benchmark only.
async fn run_concurrency_benchmark(
    host: Arc<BenchHost>,
    module: Arc<wasmtime::Module>,
    config: &BenchConfig,
) -> Result<()> {
    println!("=== Concurrency Benchmark ===\n");
    println!("Concurrency: {} parallel requests\n", config.concurrency);

    bench_concurrency::verify_concurrency(Arc::clone(&host), Arc::clone(&module)).await?;

    let results = bench_concurrency::run_concurrency_benchmark(
        Arc::clone(&host),
        Arc::clone(&module),
        config.concurrency,
    )
    .await?;

    println!("Results:");
    println!("  {}", results.total);
    println!("  {}", results.instantiation_only);
    println!("  {}", results.tap_only);
    println!();
    println!("  All succeeded: {}", results.all_succeeded);
    println!(
        "  p95 < 10ms: {}",
        results.total.p95 < Duration::from_millis(10)
    );

    Ok(())
}

/// Run async benchmark only.
async fn run_async_benchmark(wasm_path: &Path, config: &BenchConfig) -> Result<()> {
    println!("=== Async Host Functions Benchmark ===\n");
    println!("Concurrency: {} parallel requests\n", config.concurrency);

    let async_host = Arc::new(AsyncBenchHost::new()?);
    let async_module = Arc::new(async_host.compile_from_file(wasm_path)?);

    bench_async::verify_async(Arc::clone(&async_host), Arc::clone(&async_module)).await?;

    let results = bench_async::run_async_benchmark(
        Arc::clone(&async_host),
        Arc::clone(&async_module),
        config.concurrency,
    )
    .await?;

    println!("Results:");
    println!("  {}", results.total);
    println!("  Wall-clock: {:?}", results.wall_clock_time);
    println!(
        "  Completed without deadlock: {}",
        results.completed_without_deadlock
    );

    Ok(())
}

/// Test serialization performance across all payload sizes.
fn run_payload_scaling_benchmark(
    host: &Arc<BenchHost>,
    module: &Arc<wasmtime::Module>,
    config: &BenchConfig,
) -> Result<()> {
    println!("=== Payload Scaling Benchmark ===\n");
    println!("Testing serialization performance across payload sizes\n");

    let sizes = [
        PayloadSize::Small,
        PayloadSize::Medium,
        PayloadSize::Large,
        PayloadSize::XLarge,
    ];

    println!("| Payload Size | Actual Bytes | Avg Tap Time | p95 | p99 |");
    println!("|--------------|--------------|--------------|-----|-----|");

    for size in sizes {
        let actual_bytes = fixture::synthetic_item_size_for(size);

        // Run benchmark with this payload size
        // Note: Current implementation uses fixture::synthetic_item() which is Small
        // We'd need to pass size through to the benchmark functions
        let results = bench_serialize::run_serialize_benchmark(host, module, config.iterations)?;

        println!(
            "| {:12} | {:>12} | {:>12?} | {:>12?} | {:>12?} |",
            size.name(),
            actual_bytes,
            results.tap_only.per_call_avg,
            results.tap_only.p95,
            results.tap_only.p99,
        );
    }

    println!();
    println!("Note: This benchmark currently uses the small payload for all tests.");
    println!("Full payload scaling requires passing size to guest WASM.");

    Ok(())
}

/// Test mutation-heavy workload (write more fields than read).
fn run_mutation_benchmark(
    host: &Arc<BenchHost>,
    module: &Arc<wasmtime::Module>,
    config: &BenchConfig,
) -> Result<()> {
    println!("=== Mutation Benchmark ===\n");
    println!("Testing write-heavy workload performance\n");
    println!("Iterations: {}\n", config.iterations);

    // For mutation benchmarks, we compare:
    // - Handle-based: Multiple set_field calls
    // - Full-serialization: Modify JSON and return full payload

    println!("Handle-based (3 reads + 1 write per call):");
    let handle_results = bench_handle::run_handle_benchmark(host, module, config.iterations)?;
    println!("  Tap avg: {:?}", handle_results.tap_only.per_call_avg);
    println!("  Tap p95: {:?}", handle_results.tap_only.p95);
    println!();

    println!("Full-serialization (parse + modify + serialize per call):");
    let serialize_results =
        bench_serialize::run_serialize_benchmark(host, module, config.iterations)?;
    println!("  Tap avg: {:?}", serialize_results.tap_only.per_call_avg);
    println!("  Tap p95: {:?}", serialize_results.tap_only.p95);
    println!();

    let handle_avg = handle_results.tap_only.per_call_avg.as_nanos() as f64;
    let serialize_avg = serialize_results.tap_only.per_call_avg.as_nanos() as f64;

    if handle_avg < serialize_avg {
        println!(
            "Handle-based is {:.2}x faster for mutations",
            serialize_avg / handle_avg
        );
    } else {
        println!(
            "Full-serialization is {:.2}x faster for mutations",
            handle_avg / serialize_avg
        );
    }

    println!();
    println!("Note: Current workload is 3 reads + 1 write. For heavy mutations (10+ writes),");
    println!("handle-based may be more advantageous. Extended mutation benchmarks TODO.");

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
        let durations: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        let result = BenchResult::from_durations("test", &durations);

        assert_eq!(result.name, "test");
        assert_eq!(result.total_calls, 100);
        assert_eq!(result.p50, Duration::from_micros(51));
        assert_eq!(result.p95, Duration::from_micros(96));
        assert_eq!(result.p99, Duration::from_micros(100));
    }
}
