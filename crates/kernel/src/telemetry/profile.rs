//! Per-request counters, in a task-local scope.
//!
//! `track_request_timing` makes one [`RequestProfile`] per request and runs the
//! request inside [`scope`]. Each choke point adds to whatever profile is in
//! scope with a `try_with`, which is a no-op when there is none (startup, cron,
//! a queue job, a task the request spawned), so the counts are exactly the work
//! done on the request's own task, and the same on every run.

use std::future::Future;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use uuid::Uuid;

use crate::metrics::Metrics;

tokio::task_local! {
    static PROFILE: Arc<RequestProfile>;
}

/// What one request or job did so far.
#[derive(Debug, Default)]
pub struct RequestProfile {
    queries: AtomicU32,
    query_nanos: AtomicU64,
    cache_gets: AtomicU32,
    cache_hits: AtomicU32,
    cache_misses: AtomicU32,
    cache_sets: AtomicU32,
    plugin_calls: AtomicU32,
    plugin_nanos: AtomicU64,
    host_calls: AtomicU32,
    gathers: AtomicU32,
    renders: AtomicU32,
    render_nanos: AtomicU64,
    render_depth: AtomicU32,
    outgoing_calls: AtomicU32,
    /// The signed-in user, noted by the audit actor layer, so the timing layer
    /// can decide who sees the detailed `Server-Timing` after the counts are
    /// read.
    user_id: OnceLock<Uuid>,
    /// Each statement's fingerprint and normalised text (no literals), in
    /// the order they ran, when asked for.
    fingerprints: Option<Mutex<Vec<(String, String)>>>,
    /// Where query and tap durations are observed, when the profile belongs
    /// to a server with metrics.
    metrics: Option<Arc<Metrics>>,
}

/// A copy of a profile's counters at one moment.
///
/// Carried in a response's extensions, which is how the performance tests
/// read a request's numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestProfileSnapshot {
    /// Database statements run.
    pub queries: u32,
    /// Time spent in them.
    pub query_time: Duration,
    /// Cache lookups, in process and in Redis.
    pub cache_gets: u32,
    /// Lookups that found a value.
    pub cache_hits: u32,
    /// Lookups that did not.
    pub cache_misses: u32,
    /// Values written to the Redis cache.
    pub cache_sets: u32,
    /// Plugin exports called (taps and plugin-to-plugin calls).
    pub plugin_calls: u32,
    /// Time spent in them, nested calls counted in their callers too.
    pub plugin_time: Duration,
    /// Host functions plugins called.
    pub host_calls: u32,
    /// Gather queries run.
    pub gathers: u32,
    /// Template renders entered (page, item, element and admin page).
    pub renders: u32,
    /// Time spent rendering, outermost renders only.
    pub render_time: Duration,
    /// Outgoing HTTP requests.
    pub outgoing_calls: u32,
    /// Statement fingerprints in the order they ran, when they were collected.
    pub fingerprints: Vec<String>,
    /// The same statements, normalised (every literal and parameter a `?`),
    /// so a test can say which statement a fingerprint is. Never leaves the
    /// process.
    pub statements: Vec<String>,
}

impl RequestProfile {
    /// A profile that counts, observing query durations into `metrics`.
    pub fn new(metrics: Option<Arc<Metrics>>, collect_fingerprints: bool) -> Self {
        Self {
            fingerprints: collect_fingerprints.then(|| Mutex::new(Vec::new())),
            metrics,
            ..Self::default()
        }
    }

    /// Whether this profile records statement fingerprints.
    pub fn collects_fingerprints(&self) -> bool {
        self.fingerprints.is_some()
    }

    /// The user the audit actor layer noted, if any.
    pub fn user_id(&self) -> Option<Uuid> {
        self.user_id.get().copied()
    }

    /// Read every counter.
    pub fn snapshot(&self) -> RequestProfileSnapshot {
        let (fingerprints, statements): (Vec<String>, Vec<String>) = self
            .fingerprints
            .as_ref()
            .and_then(|f| f.lock().ok().map(|v| v.clone()))
            .unwrap_or_default()
            .into_iter()
            .unzip();
        let load = |a: &AtomicU32| a.load(Ordering::Relaxed);
        let nanos = |a: &AtomicU64| Duration::from_nanos(a.load(Ordering::Relaxed));
        RequestProfileSnapshot {
            queries: load(&self.queries),
            query_time: nanos(&self.query_nanos),
            cache_gets: load(&self.cache_gets),
            cache_hits: load(&self.cache_hits),
            cache_misses: load(&self.cache_misses),
            cache_sets: load(&self.cache_sets),
            plugin_calls: load(&self.plugin_calls),
            plugin_time: nanos(&self.plugin_nanos),
            host_calls: load(&self.host_calls),
            gathers: load(&self.gathers),
            renders: load(&self.renders),
            render_time: nanos(&self.render_nanos),
            outgoing_calls: load(&self.outgoing_calls),
            fingerprints,
            statements,
        }
    }
}

/// Run `fut` with `profile` as the request's profile.
pub async fn scope<F: Future>(profile: Arc<RequestProfile>, fut: F) -> F::Output {
    PROFILE.scope(profile, fut).await
}

/// Whether a profile is in scope on this task.
pub fn active() -> bool {
    PROFILE.try_with(|_| ()).is_ok()
}

/// Run `f` on the profile in scope, if there is one.
fn with(f: impl FnOnce(&RequestProfile)) {
    let _ = PROFILE.try_with(|p| f(p));
}

fn nanos_of(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Note the signed-in user of this request.
pub fn note_user(user_id: Uuid) {
    with(|p| {
        let _ = p.user_id.set(user_id);
    });
}

/// Count one statement. `sql` is only read when fingerprints are collected.
pub(crate) fn count_query(elapsed: Duration, sql: impl FnOnce() -> Option<String>) {
    with(|p| {
        p.queries.fetch_add(1, Ordering::Relaxed);
        p.query_nanos
            .fetch_add(nanos_of(elapsed), Ordering::Relaxed);
        if let Some(m) = p.metrics.as_ref() {
            m.record_db_query(elapsed.as_secs_f64());
        }
        if let Some(list) = p.fingerprints.as_ref()
            && let Some(sql) = sql()
            && let Ok(mut list) = list.lock()
        {
            let normalized = super::fingerprint::normalize(&sql);
            list.push((
                super::fingerprint::fingerprint_normalized(&normalized),
                normalized,
            ));
        }
    });
}

/// Whether the profile in scope wants statement text (to fingerprint it).
pub(crate) fn wants_statements() -> bool {
    PROFILE
        .try_with(|p| p.collects_fingerprints())
        .unwrap_or(false)
}

/// Count one cache lookup.
pub(crate) fn count_cache_get(hit: bool) {
    with(|p| {
        p.cache_gets.fetch_add(1, Ordering::Relaxed);
        if hit {
            p.cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            p.cache_misses.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// Count one value written to the Redis cache.
pub(crate) fn count_cache_set() {
    with(|p| {
        p.cache_sets.fetch_add(1, Ordering::Relaxed);
    });
}

/// Count one plugin export call that took `elapsed`, into the profile and
/// into `tap_duration_seconds`.
pub(crate) fn count_plugin_call(plugin: &str, tap: &str, elapsed: Duration) {
    with(|p| {
        if let Some(m) = p.metrics.as_ref() {
            m.record_tap(plugin, tap, elapsed.as_secs_f64());
        }
        p.plugin_calls.fetch_add(1, Ordering::Relaxed);
        p.plugin_nanos
            .fetch_add(nanos_of(elapsed), Ordering::Relaxed);
    });
}

/// Count one host call.
pub(crate) fn count_host_call() {
    with(|p| {
        p.host_calls.fetch_add(1, Ordering::Relaxed);
    });
}

/// Count one gather run.
pub(crate) fn count_gather() {
    with(|p| {
        p.gathers.fetch_add(1, Ordering::Relaxed);
    });
}

/// Count one outgoing HTTP request.
pub(crate) fn count_outgoing() {
    with(|p| {
        p.outgoing_calls.fetch_add(1, Ordering::Relaxed);
    });
}

/// Time a render; nested renders count but only the outermost adds time.
pub(crate) fn render<T>(f: impl FnOnce() -> T) -> T {
    let outer = PROFILE
        .try_with(|p| {
            p.renders.fetch_add(1, Ordering::Relaxed);
            p.render_depth.fetch_add(1, Ordering::Relaxed) == 0
        })
        .ok();
    // Undone on drop, so a render that panics does not leave the depth raised
    // for the rest of the request.
    struct Leave {
        /// Set for the outermost render only, which alone adds its time.
        started: Option<std::time::Instant>,
    }
    impl Drop for Leave {
        fn drop(&mut self) {
            with(|p| {
                p.render_depth.fetch_sub(1, Ordering::Relaxed);
                if let Some(started) = self.started {
                    p.render_nanos
                        .fetch_add(nanos_of(started.elapsed()), Ordering::Relaxed);
                }
            });
        }
    }
    let _leave = outer.map(|outermost| Leave {
        started: outermost.then(std::time::Instant::now),
    });
    f()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_land_in_the_profile_in_scope_only() {
        // Outside a profile a count is a no-op, and does not panic.
        count_cache_get(true);
        assert!(!active());

        let profile = Arc::new(RequestProfile::new(None, true));
        let wall = scope(profile.clone(), async {
            assert!(active());
            count_query(Duration::from_millis(2), || {
                Some("SELECT * FROM item WHERE id = 'x'".into())
            });
            count_cache_get(true);
            count_cache_get(false);
            count_cache_set();
            count_plugin_call("p", "tap_x", Duration::from_millis(1));
            count_host_call();
            count_gather();
            count_outgoing();
            // Only the outermost render adds its time: a nested one counted
            // too would make the total about twice the wall time.
            let started = std::time::Instant::now();
            let v = render(|| {
                render(|| {
                    std::thread::sleep(Duration::from_millis(30));
                    7
                })
            });
            let wall = started.elapsed();
            assert_eq!(v, 7);
            note_user(Uuid::nil());
            wall
        })
        .await;

        let s = profile.snapshot();
        assert_eq!(s.queries, 1);
        assert_eq!(s.query_time, Duration::from_millis(2));
        assert_eq!((s.cache_gets, s.cache_hits, s.cache_misses), (2, 1, 1));
        assert_eq!(s.cache_sets, 1);
        assert_eq!(s.plugin_calls, 1);
        assert_eq!(s.host_calls, 1);
        assert!(
            s.render_time >= Duration::from_millis(30) && s.render_time <= wall,
            "{:?} for a wall time of {wall:?}",
            s.render_time
        );
        assert_eq!(s.gathers, 1);
        assert_eq!(s.outgoing_calls, 1);
        assert_eq!(s.renders, 2);
        assert_eq!(s.fingerprints.len(), 1);
        assert!(s.fingerprints[0].starts_with("select:item:"));
        assert_eq!(profile.user_id(), Some(Uuid::nil()));
    }

    #[tokio::test]
    async fn statement_text_is_not_built_unless_fingerprints_are_wanted() {
        let profile = Arc::new(RequestProfile::new(None, false));
        scope(profile.clone(), async {
            assert!(!wants_statements());
            count_query(Duration::ZERO, || panic!("statement text was built"));
        })
        .await;
        assert_eq!(profile.snapshot().queries, 1);
    }
}
