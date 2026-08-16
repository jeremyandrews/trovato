//! OQ-11: WASM plugin-boundary performance baseline.
//!
//! This module measures the three boundary costs the PRD (FR-9) names and the
//! one threshold the architecture gates on (FR-4a / AD-2 / AD-S-4):
//!
//! 1. **Host-function call overhead** — the round-trip cost of a guest→host→guest
//!    call across the WASM boundary, isolated from loop bookkeeping by subtracting
//!    an identical empty loop, plus a realistic memory-marshaling host call.
//! 2. **Cross-boundary serialization** — the cost of marshaling a representative
//!    item payload into guest memory, parsing/handling it, and returning a result,
//!    measured across payload sizes to derive the marginal per-byte cost.
//! 3. **Memory allocation** — the guest-side allocation cost every cross-boundary
//!    payload incurs (the SDK allocates a buffer per host response and per result).
//!
//! It also measures the **end-to-end tap dispatch** (`Store::new` +
//! instantiate + marshal + call + return) against the **1ms / typical-payload**
//! trigger, on an engine configured to match the production runtime
//! (`crates/kernel/src/plugin/runtime.rs`): pooling allocator, `wasm_threads(false)`,
//! `epoch_interruption(true)`, Cranelift `Speed`. The production-faithful dispatch
//! path uses `instantiate_async`/`call_async` (Wasmtime 43 enables async implicitly).
//!
//! Statistical method: each micro-cost amortizes an inner loop of `INNER` ops over
//! a single wasm entry, repeated `ROUNDS` times after `WARMUP` discarded rounds; the
//! reported figure is the distribution across rounds (mean, median, min, p95, p99,
//! population stddev, coefficient of variation). The dispatch gate instantiates a
//! fresh instance per sample (matching production's per-dispatch model) over
//! `DISPATCH_SAMPLES` samples.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Module, PoolingAllocationConfig, Store,
    TypedFunc,
};

use crate::fixture::{self, PayloadSize};
use crate::host::{StubHostState, create_linker};

/// Inner-loop length for the amortized micro-benchmarks (host-call, alloc).
const INNER: i32 = 200_000;
/// Inner-loop length for the heavier realistic-host-call micro-benchmark.
const INNER_FIELD: i32 = 50_000;
/// Measured rounds per micro-benchmark (the reported distribution is over these).
const ROUNDS: usize = 50;
/// Discarded warm-up rounds before measurement begins.
const WARMUP: usize = 5;
/// Fresh-instance samples for the end-to-end dispatch gate.
const DISPATCH_SAMPLES: usize = 1_000;
/// Generous epoch deadline so the (production-matching) epoch checks are compiled
/// in and exercised, but never actually trip during a benchmark run.
const EPOCH_DEADLINE: u64 = u64::MAX;
/// The FR-4a / AD-2 / AD-S-4 redesign trigger: 1ms per invocation, in nanoseconds.
const GATE_NS: f64 = 1_000_000.0;

/// Summary statistics over a set of timing samples, all in nanoseconds.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    /// Number of samples.
    pub n: usize,
    /// Arithmetic mean (ns).
    pub mean: f64,
    /// Median / p50 (ns).
    pub median: f64,
    /// Minimum (ns).
    pub min: f64,
    /// 95th percentile (ns).
    pub p95: f64,
    /// 99th percentile (ns).
    pub p99: f64,
    /// Population standard deviation (ns).
    pub stddev: f64,
    /// Coefficient of variation (stddev / mean), unitless.
    pub cv: f64,
}

impl Stats {
    /// Compute summary statistics from a slice of nanosecond samples.
    ///
    /// Returns all-zero stats for an empty input.
    pub fn from_samples(samples: &[f64]) -> Self {
        let n = samples.len();
        if n == 0 {
            return Self {
                n: 0,
                mean: 0.0,
                median: 0.0,
                min: 0.0,
                p95: 0.0,
                p99: 0.0,
                stddev: 0.0,
                cv: 0.0,
            };
        }

        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let sum: f64 = sorted.iter().sum();
        let mean = sum / n as f64;
        let variance = sorted.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
        let stddev = variance.sqrt();

        Self {
            n,
            mean,
            median: percentile(&sorted, 0.50),
            min: sorted[0],
            p95: percentile(&sorted, 0.95),
            p99: percentile(&sorted, 0.99),
            stddev,
            cv: if mean > 0.0 { stddev / mean } else { 0.0 },
        }
    }

    /// Render a markdown table row (label + mean/median/p95/p99/CV) using the
    /// most readable unit for the magnitude.
    fn row(&self, label: &str) -> String {
        format!(
            "| {label} | {} | {} | {} | {} | {:.1}% |",
            fmt_ns(self.mean),
            fmt_ns(self.median),
            fmt_ns(self.p95),
            fmt_ns(self.p99),
            self.cv * 100.0,
        )
    }
}

/// Compute a percentile from already-sorted samples using nearest-rank.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// Format a nanosecond value with an adaptive unit (ns / µs / ms).
pub(crate) fn fmt_ns(ns: f64) -> String {
    if ns < 1_000.0 {
        format!("{ns:.1} ns")
    } else if ns < 1_000_000.0 {
        format!("{:.3} µs", ns / 1_000.0)
    } else {
        format!("{:.3} ms", ns / 1_000_000.0)
    }
}

/// Build a Wasmtime engine matching the production plugin runtime configuration.
///
/// Mirrors `crates/kernel/src/plugin/runtime.rs::create_engine`: pooling
/// allocator, threads disabled, epoch interruption enabled, Cranelift tuned for
/// speed. (Wasmtime 43 enables async support implicitly, so both `call` and
/// `call_async` are available on engines built this way — the production runtime
/// uses the async path for tap dispatch.)
pub(crate) fn create_production_engine() -> Result<Engine> {
    let mut config = Config::new();

    let mut pooling = PoolingAllocationConfig::default();
    pooling.total_component_instances(64);
    pooling.total_memories(64);
    pooling.total_tables(64);
    pooling.max_memory_size(64 * 1024 * 1024);
    config.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling));

    config.wasm_threads(false);
    config.epoch_interruption(true);
    config.cranelift_opt_level(wasmtime::OptLevel::Speed);

    Engine::new(&config)
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .context("failed to create production-config wasmtime engine")
}

/// Register all production host functions plus the OQ-11 `bench/noop` no-op used
/// to isolate the raw trampoline cost.
///
/// `bench/noop` now lives in the base [`create_linker`] (the guest imports it
/// unconditionally, so every host that instantiates the guest needs it); this
/// wrapper is retained for call-site clarity at the OQ-11 measurement sites.
pub(crate) fn create_oq11_linker(engine: &Engine) -> Result<Linker<StubHostState>> {
    create_linker(engine)
}

/// A store pre-loaded with a synthetic item at handle 0 (for handle-based and
/// realistic-host-call benchmarks).
fn store_with_item(engine: &Engine, size: PayloadSize) -> Store<StubHostState> {
    let mut state = StubHostState::new();
    state.load_item(0, fixture::synthetic_item_sized(size));
    Store::new(engine, state)
}

/// Run the full OQ-11 baseline and print a self-contained report to stdout.
pub async fn run_oq11(wasm_path: &Path) -> Result<()> {
    println!("=== OQ-11: WASM Plugin-Boundary Performance Baseline ===\n");
    println!("Engine config (matches crates/kernel/src/plugin/runtime.rs):");
    println!("  pooling allocator · wasm_threads(false) · epoch_interruption(true)");
    println!("  cranelift_opt_level(Speed) · async dispatch via instantiate_async/call_async\n");
    println!(
        "Method: INNER={INNER} ops/entry, {ROUNDS} measured rounds ({WARMUP} warm-up), \
         {DISPATCH_SAMPLES} fresh-instance dispatch samples.\n"
    );

    // --- Sync engine for the steady-state micro-costs ---
    let engine = create_production_engine()?;
    let linker = create_oq11_linker(&engine)?;
    let module = Module::from_file(&engine, wasm_path)
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .context("failed to compile guest module")?;

    measure_host_call(&engine, &linker, &module)?;
    measure_serialization(&engine, &linker, &module)?;
    measure_allocation(&engine, &linker, &module)?;

    // --- Dispatch gate: sync (lower bound) and async (production path) ---
    let async_engine = create_production_engine()?;
    let async_linker = create_oq11_linker(&async_engine)?;
    let async_module = Module::from_file(&async_engine, wasm_path)
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .context("failed to compile guest module (async engine)")?;

    measure_dispatch_gate(
        &engine,
        &linker,
        &module,
        &async_engine,
        &async_linker,
        &async_module,
    )
    .await?;

    Ok(())
}

/// Cost 1: host-function call overhead.
fn measure_host_call(
    engine: &Engine,
    linker: &Linker<StubHostState>,
    module: &Module,
) -> Result<()> {
    println!("--- Cost 1: Host-function call overhead ---\n");

    let mut store = store_with_item(engine, PayloadSize::Small);
    store.set_epoch_deadline(EPOCH_DEADLINE);
    let instance = linker
        .instantiate(&mut store, module)
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;

    let noop_loop: TypedFunc<i32, i32> = instance
        .get_typed_func(&mut store, "bench_noop_loop")
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let empty_loop: TypedFunc<i32, i32> = instance
        .get_typed_func(&mut store, "bench_empty_loop")
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let field_loop: TypedFunc<(i32, i32), i32> = instance
        .get_typed_func(&mut store, "bench_field_loop")
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;

    let noop = run_rounds(ROUNDS, WARMUP, INNER, |n| {
        noop_loop
            .call(&mut store, n)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        Ok(())
    })?;
    let empty = run_rounds(ROUNDS, WARMUP, INNER, |n| {
        empty_loop
            .call(&mut store, n)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        Ok(())
    })?;
    let field = run_rounds(ROUNDS, WARMUP, INNER_FIELD, |n| {
        field_loop
            .call(&mut store, (0, n))
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        Ok(())
    })?;

    let pure = (noop.mean - empty.mean).max(0.0);

    println!("| Measurement | mean | median | p95 | p99 | CV |");
    println!("|---|---|---|---|---|---|");
    println!("{}", empty.row("empty loop (baseline)"));
    println!("{}", noop.row("loop + no-op host call"));
    println!("{}", field.row("loop + get_field_string (marshaled)"));
    println!(
        "\n  → Pure host-call overhead (no-op − baseline): **{}** per call",
        fmt_ns(pure)
    );
    println!(
        "  → Realistic host call w/ memory marshaling (field_body ≈ {} B): **{}** per call\n",
        fixture::synthetic_item_sized(PayloadSize::Small)["fields"]["field_body"]["value"]
            .as_str()
            .map(str::len)
            .unwrap_or(0),
        fmt_ns(field.mean),
    );
    Ok(())
}

/// Cost 2: cross-boundary serialization, across payload sizes.
fn measure_serialization(
    engine: &Engine,
    linker: &Linker<StubHostState>,
    module: &Module,
) -> Result<()> {
    println!(
        "--- Cost 2: Cross-boundary serialization (marshal in + parse + build + read out) ---\n"
    );
    println!("| Payload | bytes | mean | median | p95 | p99 | CV |");
    println!("|---|---|---|---|---|---|---|");

    let sizes = [
        PayloadSize::Small,
        PayloadSize::Medium,
        PayloadSize::Large,
        PayloadSize::XLarge,
    ];

    let mut points: Vec<(usize, f64)> = Vec::new();

    for size in sizes {
        let json = fixture::synthetic_item_json(size);
        let bytes = json.as_bytes();

        let mut store = store_with_item(engine, size);
        store.set_epoch_deadline(EPOCH_DEADLINE);
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;

        let alloc_fn: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, "alloc")
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let tap: TypedFunc<(i32, i32), i64> = instance
            .get_typed_func(&mut store, "tap_item_view_full")
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("guest missing memory export")?;

        // Allocate one input buffer reused across rounds (excludes per-round alloc
        // from the serialization measurement).
        let ptr = alloc_fn
            .call(&mut store, bytes.len() as i32)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;

        // Each "op" is one full round-trip: copy payload in, call the tap (which
        // parses + builds), and read the returned JSON out of guest memory.
        let stats = run_rounds(ROUNDS, WARMUP, 1, |_| {
            memory.data_mut(&mut store)[ptr as usize..ptr as usize + bytes.len()]
                .copy_from_slice(bytes);
            let result = tap
                .call(&mut store, (ptr, bytes.len() as i32))
                .map_err(|e| anyhow::anyhow!("{e:#}"))?;
            let out_ptr = (result >> 32) as usize;
            let out_len = (result & 0xFFFF_FFFF) as usize;
            let data = memory.data(&store);
            // Read the output back out (host-side cost of consuming the result).
            let _ = std::hint::black_box(&data[out_ptr..out_ptr + out_len]);
            Ok(())
        })?;

        let label = size.name();
        println!(
            "| {label} | {} | {} | {} | {} | {} | {:.1}% |",
            bytes.len(),
            fmt_ns(stats.mean),
            fmt_ns(stats.median),
            fmt_ns(stats.p95),
            fmt_ns(stats.p99),
            stats.cv * 100.0,
        );
        points.push((bytes.len(), stats.mean));
    }

    // Derive the marginal per-byte / throughput cost via the smallest and largest
    // points (a two-point slope is enough to separate fixed overhead from copy cost).
    if let (Some(first), Some(last)) = (points.first(), points.last())
        && last.0 > first.0
    {
        let slope_ns_per_byte = (last.1 - first.1) / (last.0 - first.0) as f64;
        let fixed = first.1 - slope_ns_per_byte * first.0 as f64;
        let mb_per_s = if slope_ns_per_byte > 0.0 {
            1_000.0 / slope_ns_per_byte
        } else {
            f64::INFINITY
        };
        println!(
            "\n  → Marginal serialization cost ≈ **{:.3} ns/byte** ({:.0} MB/s), \
             fixed overhead ≈ {}\n",
            slope_ns_per_byte,
            mb_per_s,
            fmt_ns(fixed.max(0.0)),
        );
    }
    Ok(())
}

/// Cost 3: guest-side memory allocation across the boundary.
fn measure_allocation(
    engine: &Engine,
    linker: &Linker<StubHostState>,
    module: &Module,
) -> Result<()> {
    println!("--- Cost 3: Memory allocation (alloc + touch + free, per allocation) ---\n");
    println!("| Allocation size | mean | median | p95 | p99 | CV |");
    println!("|---|---|---|---|---|---|");

    let mut store = store_with_item(engine, PayloadSize::Small);
    store.set_epoch_deadline(EPOCH_DEADLINE);
    let instance = linker
        .instantiate(&mut store, module)
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let alloc_loop: TypedFunc<(i32, i32), i64> = instance
        .get_typed_func(&mut store, "bench_alloc_loop")
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;

    for size in [64i32, 256, 4096, 65536] {
        let stats = run_rounds(ROUNDS, WARMUP, INNER, |n| {
            alloc_loop
                .call(&mut store, (n, size))
                .map_err(|e| anyhow::anyhow!("{e:#}"))?;
            Ok(())
        })?;
        println!("{}", stats.row(&format!("{size} B")));
    }
    println!();
    Ok(())
}

/// The headline gate: end-to-end tap dispatch vs the 1ms / typical-payload trigger.
#[allow(clippy::too_many_arguments)]
async fn measure_dispatch_gate(
    engine: &Engine,
    linker: &Linker<StubHostState>,
    module: &Module,
    async_engine: &Engine,
    async_linker: &Linker<StubHostState>,
    async_module: &Module,
) -> Result<()> {
    println!(
        "--- Dispatch gate: end-to-end tap invocation vs the 1ms trigger (AD-2 / AD-S-4) ---\n"
    );
    println!(
        "Each sample = fresh Store + instantiate + marshal payload + call tap_item_view_full + \
         read result (production's per-dispatch model).\n"
    );
    println!(
        "| Path | Payload | bytes | full mean | full p95 | full p99 | instantiate p95 | call p95 |"
    );
    println!("|---|---|---|---|---|---|---|---|");

    let sizes = [PayloadSize::Small, PayloadSize::Medium, PayloadSize::Large];
    let mut worst_typical_p99 = 0.0f64;

    for size in sizes {
        let json = fixture::synthetic_item_json(size);
        let bytes = json.as_bytes();

        // Sync path (lower bound on pure boundary cost).
        let (full, inst, call) = dispatch_sync(engine, linker, module, bytes, DISPATCH_SAMPLES)?;
        println!(
            "| sync | {} | {} | {} | {} | {} | {} | {} |",
            size.name(),
            bytes.len(),
            fmt_ns(full.mean),
            fmt_ns(full.p95),
            fmt_ns(full.p99),
            fmt_ns(inst.p95),
            fmt_ns(call.p95),
        );

        // Async path (production tap-dispatch path).
        let (afull, ainst, acall) = dispatch_async(
            async_engine,
            async_linker,
            async_module,
            bytes,
            DISPATCH_SAMPLES,
        )
        .await?;
        println!(
            "| async | {} | {} | {} | {} | {} | {} | {} |",
            size.name(),
            bytes.len(),
            fmt_ns(afull.mean),
            fmt_ns(afull.p95),
            fmt_ns(afull.p99),
            fmt_ns(ainst.p95),
            fmt_ns(acall.p95),
        );

        // Typical = Small + Medium; track worst-case p99 of the async (production) path.
        if matches!(size, PayloadSize::Small | PayloadSize::Medium) {
            worst_typical_p99 = worst_typical_p99.max(afull.p99);
        }
    }

    let headroom = GATE_NS / worst_typical_p99;
    let verdict = if worst_typical_p99 < GATE_NS {
        format!(
            "✓ WITHIN BUDGET — worst typical-payload p99 (async) = {} < 1ms ({:.0}× headroom)",
            fmt_ns(worst_typical_p99),
            headroom,
        )
    } else {
        format!(
            "✗ EXCEEDS 1ms — worst typical-payload p99 (async) = {} ≥ 1ms — redesign trigger live",
            fmt_ns(worst_typical_p99),
        )
    };
    println!("\n  → {verdict}\n");
    Ok(())
}

/// Run the sync dispatch loop, returning (full, instantiate-only, call-only) stats.
fn dispatch_sync(
    engine: &Engine,
    linker: &Linker<StubHostState>,
    module: &Module,
    payload: &[u8],
    samples: usize,
) -> Result<(Stats, Stats, Stats)> {
    let mut full = Vec::with_capacity(samples);
    let mut inst = Vec::with_capacity(samples);
    let mut call = Vec::with_capacity(samples);

    for _ in 0..samples {
        let t0 = Instant::now();
        let mut store = Store::new(engine, StubHostState::new());
        store.set_epoch_deadline(EPOCH_DEADLINE);
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let alloc_fn: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, "alloc")
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let tap: TypedFunc<(i32, i32), i64> = instance
            .get_typed_func(&mut store, "tap_item_view_full")
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("guest missing memory export")?;
        let inst_ns = t0.elapsed().as_nanos() as f64;

        let c0 = Instant::now();
        let ptr = alloc_fn
            .call(&mut store, payload.len() as i32)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        memory.data_mut(&mut store)[ptr as usize..ptr as usize + payload.len()]
            .copy_from_slice(payload);
        let result = tap
            .call(&mut store, (ptr, payload.len() as i32))
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let out_ptr = (result >> 32) as usize;
        let out_len = (result & 0xFFFF_FFFF) as usize;
        let _ = std::hint::black_box(&memory.data(&store)[out_ptr..out_ptr + out_len]);
        let call_ns = c0.elapsed().as_nanos() as f64;

        full.push(inst_ns + call_ns);
        inst.push(inst_ns);
        call.push(call_ns);
    }

    Ok((
        Stats::from_samples(&full),
        Stats::from_samples(&inst),
        Stats::from_samples(&call),
    ))
}

/// Run the async dispatch loop (production path), returning (full, instantiate, call) stats.
async fn dispatch_async(
    engine: &Engine,
    linker: &Linker<StubHostState>,
    module: &Module,
    payload: &[u8],
    samples: usize,
) -> Result<(Stats, Stats, Stats)> {
    let mut full = Vec::with_capacity(samples);
    let mut inst = Vec::with_capacity(samples);
    let mut call = Vec::with_capacity(samples);

    for _ in 0..samples {
        let t0 = Instant::now();
        let mut store = Store::new(engine, StubHostState::new());
        store.set_epoch_deadline(EPOCH_DEADLINE);
        let instance = linker
            .instantiate_async(&mut store, module)
            .await
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let alloc_fn: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, "alloc")
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let tap: TypedFunc<(i32, i32), i64> = instance
            .get_typed_func(&mut store, "tap_item_view_full")
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("guest missing memory export")?;
        let inst_ns = t0.elapsed().as_nanos() as f64;

        let c0 = Instant::now();
        let ptr = alloc_fn
            .call_async(&mut store, payload.len() as i32)
            .await
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        memory.data_mut(&mut store)[ptr as usize..ptr as usize + payload.len()]
            .copy_from_slice(payload);
        let result = tap
            .call_async(&mut store, (ptr, payload.len() as i32))
            .await
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let out_ptr = (result >> 32) as usize;
        let out_len = (result & 0xFFFF_FFFF) as usize;
        let _ = std::hint::black_box(&memory.data(&store)[out_ptr..out_ptr + out_len]);
        let call_ns = c0.elapsed().as_nanos() as f64;

        full.push(inst_ns + call_ns);
        inst.push(inst_ns);
        call.push(call_ns);
    }

    Ok((
        Stats::from_samples(&full),
        Stats::from_samples(&inst),
        Stats::from_samples(&call),
    ))
}

/// Run an amortized micro-benchmark: call `op(inner)` once per round (the op runs
/// `inner` operations internally), discard `warmup` rounds, and return per-op stats
/// in nanoseconds over the measured rounds.
fn run_rounds(
    rounds: usize,
    warmup: usize,
    inner: i32,
    mut op: impl FnMut(i32) -> Result<()>,
) -> Result<Stats> {
    for _ in 0..warmup {
        op(inner)?;
    }
    let mut per_op = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let t0 = Instant::now();
        op(inner)?;
        let elapsed = t0.elapsed().as_nanos() as f64;
        per_op.push(elapsed / inner as f64);
    }
    Ok(Stats::from_samples(&per_op))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_basic() {
        let samples: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        let s = Stats::from_samples(&samples);
        assert_eq!(s.n, 100);
        assert!((s.mean - 50.5).abs() < 1e-9);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.median, 51.0);
        // Nearest-rank on (n-1): round(99 * 0.95) = 94 → samples[94] = 95.
        assert_eq!(s.p95, 95.0);
        // round(99 * 0.99) = 98 → samples[98] = 99.
        assert_eq!(s.p99, 99.0);
        assert!(s.stddev > 28.0 && s.stddev < 29.0);
    }

    #[test]
    fn stats_empty_is_zero() {
        let s = Stats::from_samples(&[]);
        assert_eq!(s.n, 0);
        assert_eq!(s.mean, 0.0);
    }

    #[test]
    fn fmt_ns_units() {
        assert!(fmt_ns(500.0).ends_with("ns"));
        assert!(fmt_ns(5_000.0).ends_with("µs"));
        assert!(fmt_ns(5_000_000.0).ends_with("ms"));
    }
}
