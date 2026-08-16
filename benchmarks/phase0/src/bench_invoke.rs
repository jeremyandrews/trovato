//! FR-4a `invoke`-path performance (OQ-11, Story 2.3).
//!
//! Measures the end-to-end overhead of a single plugin-to-plugin `invoke` against
//! the **1 ms / typical-payload** budget (AD-2 / AD-S-4 / D-13 B5), extending the
//! OQ-11 tap-boundary baseline (`bench_oq11.rs`) to the invocation path Story 2.3
//! wires up.
//!
//! # What this models
//!
//! Production `invoke` routes through `do_invoke`
//! (`crates/kernel/src/host/plugin_api.rs`):
//!
//! 1. inbound payload-size check (≤ 1 MiB),
//! 2. target lookup (`runtime.get_plugin`),
//! 3. callee consent — a scan of the target's `public_functions` allowlist,
//! 4. **the new recursion-depth check** (`parent + 1 >= MAX_INVOCATION_DEPTH`),
//! 5. `instantiate_and_call_export` — a fresh `Store`, `instantiate_async`,
//!    resolution of the requested function as an **arbitrary named export**, the
//!    JSON memory-protocol round-trip, and
//! 6. outbound result-size check.
//!
//! Step 5 is the same pooled-`Store` dispatch primitive the tap gate already
//! measures, with one difference: `invoke` resolves the export by a *runtime*
//! function-name string (here `tap_item_view_full`) rather than a fixed tap name —
//! a `get_typed_func` string lookup either way. Steps 1–4 and 6 are host-side
//! integer/`Vec`-scan bookkeeping; they are included here for fidelity but cost
//! single-digit nanoseconds relative to the tens-of-µs dispatch.
//!
//! Like the tap baseline this runs against the **stub host** (`host.rs`), not real
//! DB/cache services — it measures the boundary tax, which is the OQ-11 question,
//! not host-function workloads. Numbers are M4-Max-relative (see the baseline doc;
//! the deployment re-baseline was waived under D-13 because the headroom absorbs a
//! large CPU multiple).

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use wasmtime::{Engine, Linker, Module, Store, TypedFunc};

use crate::bench_oq11::{Stats, create_oq11_linker, create_production_engine, fmt_ns};
use crate::fixture::{self, PayloadSize};
use crate::host::StubHostState;

/// Fresh-instance samples per payload size.
const INVOKE_SAMPLES: usize = 1_000;
/// Generous epoch deadline: production-matching epoch checks are compiled in and
/// exercised, but never trip during the benchmark.
const EPOCH_DEADLINE: u64 = u64::MAX;
/// The AD-2 / AD-S-4 redesign trigger: 1 ms per invocation, in nanoseconds.
const GATE_NS: f64 = 1_000_000.0;
/// The D-13 B5 regression alarm: 250 µs p99, in nanoseconds.
const ALARM_NS: f64 = 250_000.0;
/// Frozen payload cap mirrored from `host::plugin_api::MAX_PAYLOAD_BYTES`.
const MAX_PAYLOAD_BYTES: usize = 1_048_576;
/// Frozen depth cap mirrored from `host::plugin_api::MAX_INVOCATION_DEPTH`.
const MAX_INVOCATION_DEPTH: u32 = 8;

/// Run the OQ-11 invoke-path measurement and print a self-contained report.
pub async fn run_invoke_perf(wasm_path: &Path) -> Result<()> {
    println!("=== OQ-11 invoke-path performance (FR-4a / Story 2.3) ===\n");
    println!("Engine config (matches crates/kernel/src/plugin/runtime.rs):");
    println!("  pooling allocator · wasm_threads(false) · epoch_interruption(true)");
    println!("  cranelift_opt_level(Speed) · async dispatch via instantiate_async/call_async\n");
    println!(
        "Each sample models one do_invoke: payload check + public_functions scan + \
         recursion-depth check, then fresh Store + instantiate + resolve named export \
         + memory round-trip + outbound check ({INVOKE_SAMPLES} fresh-instance samples).\n"
    );

    let engine = create_production_engine()?;
    let linker = create_oq11_linker(&engine)?;
    let module = Module::from_file(&engine, wasm_path)
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .context("failed to compile guest module (invoke-path engine)")?;

    // A representative callee manifest allowlist (Model C). `do_invoke` scans this
    // before dispatch; a 4-entry list with the target last is a fair upper bound
    // for the trivial scan cost.
    let public_functions: Vec<String> = ["alpha", "beta", "gamma", "tap_item_view_full"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    println!("| Payload | bytes | p50 | p95 | p99 | mean | CV |");
    println!("|---|---|---|---|---|---|---|");

    // "Typical payload" per B5 is ≤ 10 KB (control-message scale, not the 1 MiB
    // cap); track the worst p99 across the small + medium typical sizes.
    let sizes = [PayloadSize::Small, PayloadSize::Medium];
    let mut worst_typical_p99 = 0.0f64;

    for size in sizes {
        let json = fixture::synthetic_item_json(size);
        let stats = measure_invoke(
            &engine,
            &linker,
            &module,
            &public_functions,
            json.as_bytes(),
            INVOKE_SAMPLES,
        )
        .await?;

        println!(
            "| {} | {} | {} | {} | {} | {} | {:.1}% |",
            size.name(),
            json.len(),
            fmt_ns(stats.median),
            fmt_ns(stats.p95),
            fmt_ns(stats.p99),
            fmt_ns(stats.mean),
            stats.cv * 100.0,
        );
        worst_typical_p99 = worst_typical_p99.max(stats.p99);
    }

    println!();
    report_verdict(worst_typical_p99);
    Ok(())
}

/// One full `do_invoke` model per sample, returning the distribution of total
/// per-invoke times (ns).
async fn measure_invoke(
    engine: &Engine,
    linker: &Linker<StubHostState>,
    module: &Module,
    public_functions: &[String],
    payload: &[u8],
    samples: usize,
) -> Result<Stats> {
    // The function name `invoke` would resolve at runtime (an arbitrary published
    // export, not a compile-time-fixed tap name).
    let function = "tap_item_view_full";
    let mut full = Vec::with_capacity(samples);

    for _ in 0..samples {
        let t0 = Instant::now();

        // --- Model-C host-side pre-flight (do_invoke steps 1–4) --------------
        // 1. inbound payload cap.
        let _ = std::hint::black_box(payload.len() <= MAX_PAYLOAD_BYTES);
        // 3. callee consent: scan the public_functions allowlist.
        let allowed = std::hint::black_box(public_functions.iter().any(|f| f == function));
        debug_assert!(allowed);
        // 4. recursion bound: parent depth 0 ⇒ this dispatch runs at depth 1.
        let parent_depth = std::hint::black_box(0u32);
        let depth = parent_depth + 1;
        debug_assert!(depth < MAX_INVOCATION_DEPTH);

        // --- Dispatch primitive (instantiate_and_call_export) ---------------
        let mut store = Store::new(engine, StubHostState::new());
        // Per-`Store` resource limiter (WASM-4), attached exactly as production
        // does in `instantiate_and_call_export`: stamp the plugin name (the same
        // per-call String clone `PluginState` pays) then hand the closure to
        // `store.limiter`. Included so this measurement reflects the limiter's
        // real per-call cost rather than a limiter-free model.
        store.data_mut().limiter.plugin_name = "phase0_guest".to_string();
        store.limiter(|s| &mut s.limiter);
        store.set_epoch_deadline(EPOCH_DEADLINE);
        let instance = linker
            .instantiate_async(&mut store, module)
            .await
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let alloc_fn: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, "alloc")
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        // Resolve the requested function as a named export (string lookup) — this
        // is the invoke-specific step vs the tap gate's fixed tap name.
        let target: TypedFunc<(i32, i32), i64> = instance
            .get_typed_func(&mut store, function)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("guest missing memory export")?;

        let ptr = alloc_fn
            .call_async(&mut store, payload.len() as i32)
            .await
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        memory.data_mut(&mut store)[ptr as usize..ptr as usize + payload.len()]
            .copy_from_slice(payload);
        let result = target
            .call_async(&mut store, (ptr, payload.len() as i32))
            .await
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let out_ptr = (result >> 32) as usize;
        let out_len = (result & 0xFFFF_FFFF) as usize;

        // 6. outbound result cap + read the result back out (host consumes it).
        let _ = std::hint::black_box(out_len <= MAX_PAYLOAD_BYTES);
        let _ = std::hint::black_box(&memory.data(&store)[out_ptr..out_ptr + out_len]);

        full.push(t0.elapsed().as_nanos() as f64);
    }

    Ok(Stats::from_samples(&full))
}

/// Print the verdict against the 1 ms redesign trigger and the 250 µs B5 alarm.
fn report_verdict(worst_typical_p99: f64) {
    let headroom = GATE_NS / worst_typical_p99;
    if worst_typical_p99 < GATE_NS {
        println!(
            "  → ✓ WITHIN BUDGET — worst typical-payload invoke p99 = {} < 1 ms ({:.0}× headroom)",
            fmt_ns(worst_typical_p99),
            headroom,
        );
    } else {
        println!(
            "  → ✗ EXCEEDS 1 ms — worst typical-payload invoke p99 = {} ≥ 1 ms — \
             redesign trigger live (freeze-affecting; document mitigation before FR-4 freeze)",
            fmt_ns(worst_typical_p99),
        );
    }

    if worst_typical_p99 <= ALARM_NS {
        println!(
            "  → ✓ Under the B5 regression alarm ({} ≤ 250 µs p99)",
            fmt_ns(worst_typical_p99),
        );
    } else {
        println!(
            "  → ⚠ Over the B5 regression alarm ({} > 250 µs p99) — investigate before freeze",
            fmt_ns(worst_typical_p99),
        );
    }
}
