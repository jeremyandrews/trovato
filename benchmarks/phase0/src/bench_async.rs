//! Benchmark 3: Async host functions (WASM -> Rust -> SQLx bridge).
//!
//! Validates that async host function calls from WASM guests work
//! correctly under the Tokio runtime without deadlocks. Each tap call
//! executes a (stubbed) database query via an async host function.
//!
//! Target: no deadlocks; 100 concurrent calls complete in ~1-2s with 10ms simulated delay.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::task::JoinSet;
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Module, PoolingAllocationConfig, Store,
};

use crate::BenchResult;
use crate::fixture::synthetic_item;
use crate::host::StubHostState;

/// Results from the async benchmark.
pub struct AsyncBenchmarkResults {
    /// Total request time (instantiation + async tap call).
    pub total: BenchResult,
    /// Number of concurrent requests.
    pub concurrency: u32,
    /// Whether all requests completed without deadlock.
    pub completed_without_deadlock: bool,
    /// Total wall-clock time for all concurrent requests.
    pub wall_clock_time: Duration,
}

/// Async benchmark host with async-enabled engine.
pub struct AsyncBenchHost {
    pub engine: Engine,
    pub linker: Linker<StubHostState>,
}

impl AsyncBenchHost {
    /// Create a new async benchmark host.
    pub fn new() -> Result<Self> {
        // Configure engine with async support
        let mut config = Config::new();
        config.async_support(true);

        // Pooling allocator for efficient instantiation
        let mut pooling_config = PoolingAllocationConfig::default();
        pooling_config.total_component_instances(1000);
        pooling_config.total_memories(1000);
        pooling_config.total_tables(1000);
        pooling_config.max_memory_size(64 * 1024 * 1024); // 64MB

        config.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling_config));
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);

        let engine = Engine::new(&config).context("failed to create async engine")?;
        let linker = Self::create_async_linker(&engine)?;

        Ok(Self { engine, linker })
    }

    /// Create a linker with async host functions.
    fn create_async_linker(engine: &Engine) -> Result<Linker<StubHostState>> {
        let mut linker = Linker::new(engine);

        // =========================================================================
        // Async Database Query (simulates SQLx)
        // =========================================================================

        // db_query_async(query_ptr, query_len) -> i64 (ptr << 32 | len)
        // Simulates a 10ms database query delay
        linker.func_wrap_async(
            "trovato:kernel/database",
            "db_query_async",
            |mut caller: wasmtime::Caller<'_, StubHostState>,
             (_query_ptr, _query_len): (i32, i32)|
             -> Box<dyn std::future::Future<Output = i64> + Send> {
                Box::new(async move {
                    // Simulate database query latency
                    tokio::time::sleep(Duration::from_millis(10)).await;

                    // Return an empty JSON array result
                    // In a real implementation, we'd read the query, execute it, and write results
                    let result = b"[]";

                    // Get memory and write result
                    let memory = match caller.get_export("memory") {
                        Some(wasmtime::Extern::Memory(mem)) => mem,
                        _ => return 0i64,
                    };

                    // Get alloc function to allocate space for result
                    let alloc_fn = match caller.get_export("alloc") {
                        Some(wasmtime::Extern::Func(f)) => f,
                        _ => return 0i64,
                    };

                    // Allocate space for result
                    let alloc_typed = match alloc_fn.typed::<i32, i32>(&caller) {
                        Ok(f) => f,
                        Err(_) => return 0i64,
                    };

                    let ptr = match alloc_typed
                        .call_async(&mut caller, result.len() as i32)
                        .await
                    {
                        Ok(p) => p,
                        Err(_) => return 0i64,
                    };

                    // Write result to memory
                    let data = memory.data_mut(&mut caller);
                    let start = ptr as usize;
                    let end = start + result.len();
                    if end <= data.len() {
                        data[start..end].copy_from_slice(result);
                    }

                    // Return ptr << 32 | len
                    ((ptr as i64) << 32) | (result.len() as i64)
                })
            },
        )?;

        // =========================================================================
        // Sync host functions (same as regular host)
        // =========================================================================

        // get_title (sync)
        linker.func_wrap(
            "trovato:kernel/item-api",
            "get_title",
            |mut caller: wasmtime::Caller<'_, StubHostState>,
             handle: i32,
             buf_ptr: i32,
             buf_len: i32|
             -> i32 {
                let title = match caller.data().get_title(handle) {
                    Some(t) => t,
                    None => return -1,
                };

                let memory = match caller.get_export("memory") {
                    Some(wasmtime::Extern::Memory(mem)) => mem,
                    _ => return -1,
                };

                let bytes = title.as_bytes();
                let write_len = bytes.len().min(buf_len as usize);
                let data = memory.data_mut(&mut caller);
                let start = buf_ptr as usize;
                let end = start + write_len;
                if end <= data.len() {
                    data[start..end].copy_from_slice(&bytes[..write_len]);
                    write_len as i32
                } else {
                    -1
                }
            },
        )?;

        // get_field_string (sync)
        linker.func_wrap(
            "trovato:kernel/item-api",
            "get_field_string",
            |mut caller: wasmtime::Caller<'_, StubHostState>,
             handle: i32,
             field_ptr: i32,
             field_len: i32,
             buf_ptr: i32,
             buf_len: i32|
             -> i32 {
                let memory = match caller.get_export("memory") {
                    Some(wasmtime::Extern::Memory(mem)) => mem,
                    _ => return -1,
                };

                // Read field name
                let data = memory.data(&caller);
                let field_start = field_ptr as usize;
                let field_end = field_start + field_len as usize;
                if field_end > data.len() {
                    return -1;
                }
                let field_name = match std::str::from_utf8(&data[field_start..field_end]) {
                    Ok(s) => s.to_string(),
                    Err(_) => return -1,
                };

                let value = match caller.data().get_field_string(handle, &field_name) {
                    Some(v) => v,
                    None => return -1,
                };

                let bytes = value.as_bytes();
                let write_len = bytes.len().min(buf_len as usize);
                let data = memory.data_mut(&mut caller);
                let start = buf_ptr as usize;
                let end = start + write_len;
                if end <= data.len() {
                    data[start..end].copy_from_slice(&bytes[..write_len]);
                    write_len as i32
                } else {
                    -1
                }
            },
        )?;

        // set_field_string (sync)
        linker.func_wrap(
            "trovato:kernel/item-api",
            "set_field_string",
            |mut caller: wasmtime::Caller<'_, StubHostState>,
             handle: i32,
             field_ptr: i32,
             field_len: i32,
             value_ptr: i32,
             value_len: i32| {
                let memory = match caller.get_export("memory") {
                    Some(wasmtime::Extern::Memory(mem)) => mem,
                    _ => return,
                };

                let data = memory.data(&caller);

                // Read field name
                let field_start = field_ptr as usize;
                let field_end = field_start + field_len as usize;
                if field_end > data.len() {
                    return;
                }
                let field_name = match std::str::from_utf8(&data[field_start..field_end]) {
                    Ok(s) => s.to_string(),
                    Err(_) => return,
                };

                // Read value
                let value_start = value_ptr as usize;
                let value_end = value_start + value_len as usize;
                if value_end > data.len() {
                    return;
                }
                let value = match std::str::from_utf8(&data[value_start..value_end]) {
                    Ok(s) => s.to_string(),
                    Err(_) => return,
                };

                caller
                    .data_mut()
                    .set_field_string(handle, &field_name, &value);
            },
        )?;

        // memcmp (for wee_alloc compatibility)
        linker.func_wrap(
            "env",
            "memcmp",
            |mut caller: wasmtime::Caller<'_, StubHostState>, a: i32, b: i32, n: i32| -> i32 {
                let memory = match caller.get_export("memory") {
                    Some(wasmtime::Extern::Memory(mem)) => mem,
                    _ => return 0,
                };
                let data = memory.data(&caller);
                let slice_a = &data[a as usize..(a + n) as usize];
                let slice_b = &data[b as usize..(b + n) as usize];
                match slice_a.cmp(slice_b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }
            },
        )?;

        Ok(linker)
    }

    /// Create a new store.
    pub fn create_store(&self) -> Store<StubHostState> {
        Store::new(&self.engine, StubHostState::new())
    }

    /// Create a store with pre-populated state.
    pub fn create_store_with_state(&self, state: StubHostState) -> Store<StubHostState> {
        Store::new(&self.engine, state)
    }

    /// Compile a module from file.
    pub fn compile_from_file(&self, path: &std::path::Path) -> Result<Module> {
        Module::from_file(&self.engine, path)
            .with_context(|| format!("failed to compile module from {}", path.display()))
    }
}

/// Run the async benchmark.
///
/// Spawns `concurrency` parallel tasks, each:
/// 1. Creating a new Store
/// 2. Async-instantiating the plugin
/// 3. Calling tap_item_view_async (which calls db_query_async)
/// 4. Returning the result
///
/// Validates no deadlocks occur and measures total wall-clock time.
pub async fn run_async_benchmark(
    host: Arc<AsyncBenchHost>,
    module: Arc<Module>,
    concurrency: u32,
) -> Result<AsyncBenchmarkResults> {
    let wall_clock_start = Instant::now();

    let mut join_set: JoinSet<Result<Duration>> = JoinSet::new();

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

            // Async instantiate the plugin
            let instance = host
                .linker
                .instantiate_async(&mut store, &module)
                .await
                .context("failed to instantiate plugin")?;

            // Get the tap function (uses async db_query internally)
            let tap_item_view: wasmtime::TypedFunc<i32, i64> = instance
                .get_typed_func(&mut store, "tap_item_view")
                .context("failed to get tap_item_view")?;

            // Call tap (this will internally call db_query_async)
            let result = tap_item_view.call_async(&mut store, 0).await?;

            let total_elapsed = total_start.elapsed();

            // Verify we got a result
            let len = (result & 0xFFFFFFFF) as i32;
            anyhow::ensure!(len > 0, "tap_item_view should return non-empty JSON");

            Ok(total_elapsed)
        });
    }

    // Set a timeout to detect deadlocks
    let timeout = Duration::from_secs(30);
    let deadline = Instant::now() + timeout;

    // Collect results
    let mut total_durations = Vec::with_capacity(concurrency as usize);
    let mut completed_without_deadlock = true;

    while let Some(result) = join_set.join_next().await {
        if Instant::now() > deadline {
            tracing::error!("Timeout exceeded - possible deadlock");
            completed_without_deadlock = false;
            break;
        }

        match result {
            Ok(Ok(total)) => {
                total_durations.push(total);
            }
            Ok(Err(e)) => {
                tracing::error!("Task failed: {}", e);
                completed_without_deadlock = false;
            }
            Err(e) => {
                tracing::error!("Task panicked: {}", e);
                completed_without_deadlock = false;
            }
        }
    }

    let wall_clock_time = wall_clock_start.elapsed();

    // Sort for percentile calculation
    total_durations.sort();

    Ok(AsyncBenchmarkResults {
        total: BenchResult::from_durations("async (total)", &total_durations),
        concurrency,
        completed_without_deadlock,
        wall_clock_time,
    })
}

/// Verify that async host functions work without deadlock.
pub async fn verify_async(host: Arc<AsyncBenchHost>, module: Arc<Module>) -> Result<()> {
    // Run a small async test
    let results = run_async_benchmark(host, module, 5).await?;

    anyhow::ensure!(
        results.completed_without_deadlock,
        "Async benchmark deadlocked"
    );

    println!(
        "  Async verification: {} requests completed in {:?}",
        results.total.total_calls, results.wall_clock_time
    );
    println!("  Average total time: {:?}", results.total.per_call_avg);

    Ok(())
}
