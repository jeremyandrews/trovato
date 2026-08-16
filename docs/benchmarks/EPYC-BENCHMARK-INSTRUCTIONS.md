# Phase 0 Extended Benchmarks: EPYC Instructions

These instructions will run the extended Phase 0 benchmarks on x86-64 hardware (AMD EPYC).

## Prerequisites

1. Rust toolchain with wasm32-wasip1 target:
```bash
rustup target add wasm32-wasip1
```

2. Clone and enter the repository:
```bash
git clone <repo-url>
cd trovato
```

## Building

### Step 1: Build the WASM guest plugin (release mode)
```bash
cargo build --target wasm32-wasip1 -p phase0-guest --release
```

### Step 2: Build the benchmark host (release mode)
```bash
cargo build --release -p trovato-phase0
```

## Running Benchmarks

### Quick Start: Run All Gate Benchmarks
```bash
cargo run --release -p trovato-phase0
```

This runs all three gates with default settings (500 iterations, 100 concurrency).

### Full Benchmark Suite (Recommended)

Run these commands and save the output:

```bash
# 1. All gates with default settings (baseline)
cargo run --release -p trovato-phase0 -- --benchmark all 2>&1 | tee results-all-default.txt

# 2. High concurrency test (1000 parallel requests)
cargo run --release -p trovato-phase0 -- --benchmark concurrency --concurrency 1000 2>&1 | tee results-concurrency-1000.txt

# 3. Stress test (2000 parallel requests)
cargo run --release -p trovato-phase0 -- --benchmark concurrency --concurrency 2000 2>&1 | tee results-concurrency-2000.txt

# 4. Payload scaling (tests all sizes: 2.4KB, 10KB, 50KB, 100KB)
cargo run --release -p trovato-phase0 -- --benchmark payload --iterations 200 2>&1 | tee results-payload-scaling.txt

# 5. Mutation benchmark (write-heavy workload)
cargo run --release -p trovato-phase0 -- --benchmark mutation --iterations 500 2>&1 | tee results-mutation.txt

# 6. Extended iterations for stable measurements
cargo run --release -p trovato-phase0 -- --benchmark all --iterations 1000 2>&1 | tee results-all-1000iter.txt
```

## CLI Reference

```
OPTIONS:
    -b, --benchmark <TYPE>     Benchmark to run [default: all]
                               Types: all, handle, serialize, concurrency,
                                      async, payload, mutation
    -p, --payload <SIZE>       Payload size [default: small]
                               Sizes: small (~2.4KB), medium (~10KB),
                                      large (~50KB), xlarge (~100KB)
    -i, --iterations <N>       Iterations for sequential benchmarks [default: 500]
    -c, --concurrency <N>      Concurrent tasks [default: 100]
    -h, --help                 Print help message
```

## What We're Measuring

| Benchmark | What It Tests | Success Criteria |
|-----------|---------------|------------------|
| `handle` | Handle-based WASM↔host calls | Baseline for comparison |
| `serialize` | Full JSON serialization | Compare with handle-based |
| `concurrency` | Parallel WASM instantiation | p95 < 10ms |
| `async` | Async host functions under Tokio | No deadlocks |
| `payload` | Different payload sizes | Scaling characteristics |
| `mutation` | Write-heavy workloads | Handle vs serialize for writes |

## Expected Output Format

The benchmarks will output results like:

```
=== Gate 1: Handle-Based vs Full-Serialization ===

Handle-based:
  Tap avg: 27.16µs
  p50: 26.00µs, p95: 33.75µs, p99: 43.00µs

Full-serialization:
  Tap avg: 23.39µs
  p50: 22.50µs, p95: 29.29µs, p99: 36.92µs

Speedup ratio: 0.86x
✗ GATE 1 FAILED: Full-serialization is 1.2x faster
```

## Collecting System Info

Please also capture system information:
```bash
# CPU info
lscpu | tee system-info.txt

# Kernel and OS
uname -a >> system-info.txt

# Memory
free -h >> system-info.txt

# Rust version
rustc --version >> system-info.txt
cargo --version >> system-info.txt
```

## What to Share

Please share:
1. All `results-*.txt` files
2. `system-info.txt`
3. Any errors encountered

## Troubleshooting

### "Guest plugin not found"
Build the WASM guest first:
```bash
cargo build --target wasm32-wasip1 -p phase0-guest --release
```

### Out of memory on high concurrency
Reduce concurrency or increase system limits:
```bash
ulimit -n 65535  # Increase file descriptors
```

### Slow first run
First run compiles dependencies. Subsequent runs will be faster.
