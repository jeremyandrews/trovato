#![allow(clippy::unwrap_used, clippy::expect_used)]
//! With tracing off, a Gander span costs no allocation (BG14).
//!
//! The server's own subscriber (`telemetry::subscriber`, as `main` builds it:
//! the `fmt` layer under the default `RUST_LOG`, the query listener and the
//! site log) is installed without export, which is exactly what a server with
//! `TRACING_EXPORT=off` runs. Then the kernel's span constructors are called
//! many times on one thread under a counting allocator: none of them may
//! allocate, and every span must come back disabled. The same subscriber with
//! export on (to an in-memory exporter) is then installed, to show the same
//! code does allocate when tracing is on, so the measurement can tell the two
//! apart.
//!
//! Its own test binary, because it needs its own global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing_subscriber::filter::EnvFilter;
use trovato_kernel::system_log::SystemLogLayer;
use trovato_kernel::telemetry::subscriber;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// The system allocator, counting allocations made on each thread.
struct Counting;

// SAFETY: every method forwards to `System` unchanged; the counter is a
// thread-local `Cell` that allocates nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: forwarded as received.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded as received.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: forwarded as received.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// The Gander spans and events reachable from outside the crate, once each:
/// the outgoing call span, the render span, and the in-process cache lookup
/// event and Redis set counter, which run on every cache lookup. The other
/// spans (`cache`, `plugin.tap`, `plugin.host_call`, `gather.execute`,
/// `queue.job`, `http.request`) are built with the same `trace_span!` and
/// lazily evaluated fields.
fn make_spans(metrics: &trovato_kernel::metrics::Metrics) -> bool {
    use trovato_kernel::metrics::CacheName;
    metrics.count_cache(CacheName::Item, true);
    metrics.count_cache(CacheName::User, false);
    metrics.count_cache_set(CacheName::LayerL2);
    let mut all_disabled = true;
    let span = trovato_kernel::telemetry::outgoing_span("GET", "https://api.example.com/x");
    all_disabled &= span.is_disabled();
    drop(span);
    let rendered = trovato_kernel::telemetry::render("page", || {
        // The span the render opened is the current one while it runs.
        tracing::Span::current().is_disabled()
    });
    all_disabled &= rendered;
    all_disabled
}

fn production_filter() -> EnvFilter {
    EnvFilter::new(trovato_kernel::telemetry::DEFAULT_LOG_FILTER)
}

#[test]
fn tracing_off_allocates_nothing_for_a_span_and_tracing_on_does() {
    let metrics = trovato_kernel::metrics::Metrics::new();
    // Tracing off: the production subscriber.
    let (log, _log_rows) = SystemLogLayer::new();
    let off = subscriber(std::io::sink, production_filter(), log, None);
    let off_count = tracing::dispatcher::with_default(&off, || {
        // Warm up: the first call registers each callsite's interest.
        make_spans(&metrics);
        let before = allocations();
        let mut disabled = true;
        for _ in 0..10_000 {
            disabled &= make_spans(&metrics);
        }
        let spent = allocations() - before;
        assert!(disabled, "a Gander span was enabled with tracing off");
        spent
    });
    assert_eq!(
        off_count, 0,
        "10,000 rounds of Gander spans allocated {off_count} times with tracing off"
    );

    // Tracing on: the same subscriber, exporting. The same calls now make
    // real spans, which the registry stores, so the counter has to move.
    let (log, _log_rows) = SystemLogLayer::new();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(InMemorySpanExporter::default())
        .build();
    let on = subscriber(std::io::sink, production_filter(), log, Some(provider));
    let on_count = tracing::dispatcher::with_default(&on, || {
        make_spans(&metrics);
        let before = allocations();
        let mut disabled = true;
        for _ in 0..100 {
            disabled &= make_spans(&metrics);
        }
        assert!(
            !disabled,
            "Gander spans stayed disabled with a layer that wants them"
        );
        allocations() - before
    });
    assert!(
        on_count > 0,
        "tracing on allocated nothing; the counter is not measuring"
    );
}
