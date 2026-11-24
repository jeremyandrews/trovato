#![allow(clippy::unwrap_used, clippy::expect_used)]
//! With tracing off, a Gander span costs no allocation (BG14).
//!
//! The production subscriber (the `fmt` layer under the default `RUST_LOG`,
//! plus the query listener) is installed, which is exactly what a server with
//! `TRACING_EXPORT=off` runs. Then the kernel's span constructors are called
//! many times on one thread under a counting allocator: none of them may
//! allocate, and every span must come back disabled. A layer interested in
//! the `gander` target is then added, to show the same code does allocate
//! when tracing is on, so the measurement can tell the two apart.
//!
//! Its own test binary, because it needs its own global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use tracing_subscriber::Layer;
use tracing_subscriber::filter::{EnvFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;

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

/// Every Gander span constructor a test can reach, once.
fn make_spans() -> bool {
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
    EnvFilter::new("info,tower_http=debug,sqlx=warn")
}

#[test]
fn tracing_off_allocates_nothing_for_a_span_and_tracing_on_does() {
    // Tracing off: the production subscriber.
    let off = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_filter(production_filter()),
        )
        .with(trovato_kernel::telemetry::query_listener::query_listener());
    let off_count = tracing::subscriber::with_default(off, || {
        // Warm up: the first call registers each callsite's interest.
        make_spans();
        let before = allocations();
        let mut disabled = true;
        for _ in 0..10_000 {
            disabled &= make_spans();
        }
        let spent = allocations() - before;
        assert!(disabled, "a Gander span was enabled with tracing off");
        spent
    });
    assert_eq!(
        off_count, 0,
        "10,000 rounds of Gander spans allocated {off_count} times with tracing off"
    );

    // Tracing on: something interested in `gander`. The same calls now make
    // real spans, which the registry stores, so the counter has to move.
    let on = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_filter(production_filter()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_filter(Targets::new().with_target("gander", tracing::Level::TRACE)),
        );
    let on_count = tracing::subscriber::with_default(on, || {
        make_spans();
        let before = allocations();
        let mut disabled = true;
        for _ in 0..100 {
            disabled &= make_spans();
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
