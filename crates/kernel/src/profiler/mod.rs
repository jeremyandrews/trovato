//! The request profiler: where a request's time went.
//!
//! Off by default (`PROFILER_ENABLED`). When on, one `tracing` layer,
//! [`layer`], collects each request's spans and the statements sqlx reports
//! into a [`RequestProfile`]: the phases as a tree (access checks, the
//! handler, gathers, plugin tap calls, rendering), every SQL statement with
//! its duration and its literals redacted, the tap calls by plugin, and the
//! cache hits and misses per cache. Finished profiles go into a bounded,
//! in-memory ring, [`ProfileStore`], read by the two pages under
//! `/admin/reports/requests` (`routes::admin_profiler`). Nothing is persisted.
//!
//! The instrumentation is ordinary `tracing`: spans under the one target
//! [`TARGET`], at `TRACE`, which the console filter does not enable. Off, the
//! layer is not installed, so those callsites are disabled and a request does
//! no profiling work at all; see `docs/profiler.md` for the measurement.
//!
//! What a profile never holds, by construction rather than by scrubbing: a
//! request's headers, cookies, session, body or query string (the root span
//! records the method, the matched route template and the status, nothing
//! else), cache keys (the cache event carries the cache's name and whether it
//! hit), and tap payloads (a tap span carries the plugin and the tap name).

mod collector;
mod redact;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tracing::Span;

pub use collector::{ProfilerLayer, layer};
pub use redact::redact_sql;

/// The `tracing` target every profiler span and event is emitted under;
/// `collector` filters on this value.
pub const TARGET: &str = "trovato::profile";

/// Where the profiler's own pages live. Requests to them are not profiled.
pub const PAGES: &str = "/admin/reports/requests";

/// `PROFILER_CAPTURE` default: how many recent requests are kept.
pub const DEFAULT_CAPTURE: usize = 100;

/// The most requests `PROFILER_CAPTURE` may ask to keep.
pub const MAX_CAPTURE: usize = 10_000;

/// The most phases, statements or tap calls one profile keeps of each.
///
/// Counts and totals go on past it; the entries stop and the profile is marked
/// [`RequestProfile::truncated`].
pub const MAX_ENTRIES: usize = 250;

/// The longest statement text a profile keeps, in characters.
pub const MAX_STATEMENT_CHARS: usize = 2_000;

/// The permission the profiler pages ask for.
pub const PERMISSION: &str = "view request profiles";

/// The profiler's settings, resolved once by `Config::from_env`.
#[derive(Debug, Clone)]
pub struct ProfilerConfig {
    /// `PROFILER_ENABLED`: install the layer and serve the pages.
    pub enabled: bool,
    /// `PROFILER_CAPTURE`: how many recent requests the ring keeps.
    pub capture: usize,
    /// `PROFILER_SHOW_SQL_VALUES`: keep SQL literals instead of redacting them.
    pub show_sql_values: bool,
}

impl Default for ProfilerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            capture: DEFAULT_CAPTURE,
            show_sql_values: false,
        }
    }
}

/// The root span of a profiled request.
///
/// `route` is the matched route template, never the request path. A request
/// to the profiler's own pages gets no span, so reading the profiles does not
/// push them out of the ring. With the profiler off the callsite is disabled
/// and this is [`Span::none`], which costs nothing to enter or record into.
pub fn request_span(method: &str, route: &str) -> Span {
    if route.starts_with(PAGES) {
        return Span::none();
    }
    tracing::trace_span!(
        target: TARGET,
        "request",
        method = method,
        route = route,
        status = tracing::field::Empty,
    )
}

/// Record the response status on a [`request_span`].
pub fn record_status(span: &Span, status: u16) {
    span.record("status", status);
}

/// A span for one phase of a request: `access`, `handler`, `gather`,
/// `render`. `detail` says which one (a permission, a query id, a template
/// name) and is shown beside the phase; it must never carry request data.
pub fn phase(name: &'static str, detail: &str) -> Span {
    tracing::trace_span!(target: TARGET, "phase", phase = name, detail = detail)
}

/// A span for one plugin's handler of one tap.
pub fn tap_span(plugin: &str, tap: &str) -> Span {
    tracing::trace_span!(target: TARGET, "tap", plugin = plugin, tap = tap)
}

/// One phase of a request, in start order; `depth` places it in the tree.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseEntry {
    /// `handler`, `access`, `gather`, `render` or `tap`.
    pub name: String,
    /// Which one: a permission, a query id, a template, `plugin::tap`.
    pub detail: String,
    /// 0 for a phase directly under the request.
    pub depth: usize,
    /// When it started, in milliseconds from the start of the request.
    pub start_ms: f64,
    /// How long it took, in milliseconds.
    pub duration_ms: f64,
    /// Statements run directly inside it (not in a phase below it).
    pub queries: u32,
}

/// One SQL statement.
#[derive(Debug, Clone, Serialize)]
pub struct QueryEntry {
    /// The statement, whitespace collapsed and literals replaced by `?`
    /// unless `PROFILER_SHOW_SQL_VALUES` is set.
    pub statement: String,
    /// How long sqlx reports it took, in milliseconds.
    pub duration_ms: f64,
    /// Rows it returned.
    pub rows: u64,
    /// The phase it ran in.
    pub phase: String,
}

/// One plugin's handler of one tap.
#[derive(Debug, Clone, Serialize)]
pub struct TapEntry {
    /// The plugin.
    pub plugin: String,
    /// The tap.
    pub tap: String,
    /// How long the call took, in milliseconds.
    pub duration_ms: f64,
}

/// Hits and misses in one cache.
#[derive(Debug, Clone, Serialize)]
pub struct CacheEntry {
    /// The cache, as `cache_hits_total` labels it.
    pub cache: &'static str,
    /// Lookups that found an entry.
    pub hits: u32,
    /// Lookups that did not.
    pub misses: u32,
}

/// Where one request's time went.
#[derive(Debug, Clone, Serialize)]
pub struct RequestProfile {
    /// Its address in the ring: `/admin/reports/requests/{id}`.
    pub id: u64,
    /// When it finished, UTC, `YYYY-MM-DD HH:MM:SS`.
    pub at: String,
    /// The HTTP method.
    pub method: String,
    /// The matched route template, or `<fallback>` for a request the fallback
    /// answered (an alias or a 404).
    pub route: String,
    /// The response status.
    pub status: u16,
    /// From the outermost layer in to the response, in milliseconds.
    pub total_ms: f64,
    /// The phases, in start order.
    pub phases: Vec<PhaseEntry>,
    /// Every statement run, including any past [`MAX_ENTRIES`].
    pub query_count: u32,
    /// Their total duration, in milliseconds.
    pub query_ms: f64,
    /// The statements, up to [`MAX_ENTRIES`].
    pub queries: Vec<QueryEntry>,
    /// The tap calls, up to [`MAX_ENTRIES`].
    pub taps: Vec<TapEntry>,
    /// Per cache, every cache that was read.
    pub caches: Vec<CacheEntry>,
    /// Cache hits across every cache.
    pub cache_hits: u32,
    /// Cache misses across every cache.
    pub cache_misses: u32,
    /// Whether an entry list reached [`MAX_ENTRIES`] and stopped.
    pub truncated: bool,
}

/// The recent request profiles: a bounded ring, newest first, in memory, per
/// process.
#[derive(Debug)]
pub struct ProfileStore {
    capacity: usize,
    show_sql_values: bool,
    next_id: AtomicU64,
    ring: Mutex<VecDeque<Arc<RequestProfile>>>,
}

impl ProfileStore {
    /// A store for `config`, or `None` when the profiler is off.
    pub fn for_config(config: &ProfilerConfig) -> Option<Arc<Self>> {
        config.enabled.then(|| {
            Arc::new(Self {
                capacity: config.capture.clamp(1, MAX_CAPTURE),
                show_sql_values: config.show_sql_values,
                next_id: AtomicU64::new(1),
                ring: Mutex::new(VecDeque::new()),
            })
        })
    }

    /// How many requests it keeps.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Whether statements keep their literals.
    pub fn show_sql_values(&self) -> bool {
        self.show_sql_values
    }

    /// The stored profiles, newest first.
    pub fn recent(&self) -> Vec<Arc<RequestProfile>> {
        self.ring().iter().cloned().collect()
    }

    /// The stored profile `id`, if it is still in the ring.
    pub fn get(&self, id: u64) -> Option<Arc<RequestProfile>> {
        self.ring().iter().find(|p| p.id == id).cloned()
    }

    /// Number `profile` and store it, dropping the oldest past capacity.
    fn push(&self, mut profile: RequestProfile) {
        profile.id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut ring = self.ring();
        ring.push_front(Arc::new(profile));
        ring.truncate(self.capacity);
    }

    /// The ring, whatever a panicking writer left: every write is one push and
    /// one truncate, so a poisoned lock still guards a well-formed ring.
    fn ring(&self) -> std::sync::MutexGuard<'_, VecDeque<Arc<RequestProfile>>> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn profile(route: &str) -> RequestProfile {
        RequestProfile {
            id: 0,
            at: String::new(),
            method: "GET".to_string(),
            route: route.to_string(),
            status: 200,
            total_ms: 0.0,
            phases: Vec::new(),
            query_count: 0,
            query_ms: 0.0,
            queries: Vec::new(),
            taps: Vec::new(),
            caches: Vec::new(),
            cache_hits: 0,
            cache_misses: 0,
            truncated: false,
        }
    }

    #[test]
    fn off_means_no_store() {
        assert!(ProfileStore::for_config(&ProfilerConfig::default()).is_none());
    }

    /// The ring keeps the newest `capture` profiles and forgets the rest.
    #[test]
    fn the_ring_is_bounded_and_newest_first() {
        let store = ProfileStore::for_config(&ProfilerConfig {
            enabled: true,
            capture: 2,
            show_sql_values: false,
        })
        .unwrap();
        for route in ["/a", "/b", "/c"] {
            store.push(profile(route));
        }
        let routes: Vec<String> = store.recent().iter().map(|p| p.route.clone()).collect();
        assert_eq!(routes, ["/c", "/b"]);
        assert!(store.get(1).is_none(), "the oldest was dropped");
        assert_eq!(store.get(3).unwrap().route, "/c");
    }

    /// The profiler's own pages are never profiled.
    #[test]
    fn the_profiler_pages_get_no_span() {
        assert!(request_span("GET", PAGES).is_none());
        assert!(request_span("GET", &format!("{PAGES}/{{id}}")).is_none());
    }
}
