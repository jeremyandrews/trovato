//! The one collector: a `tracing_subscriber::Layer` that turns a request's
//! spans and sqlx's statement events into a [`RequestProfile`].
//!
//! It sees only what its own filter lets through: spans and events on
//! [`TARGET`], and sqlx's `sqlx::query` statement events. Per-layer filtering
//! keeps its spans out of the console's view and the console's out of its, so
//! a phase's parent is always another profiler span.
//!
//! The same callbacks record `tap_duration_seconds` and
//! `db_query_duration_seconds`, for every tap call and statement whether or not
//! it ran inside a request, so those histograms move whenever the profiler is
//! on.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{Filtered, Targets};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::{LookupSpan, SpanRef};

use super::{
    CacheEntry, MAX_ENTRIES, MAX_STATEMENT_CHARS, PhaseEntry, ProfileStore, QueryEntry,
    RequestProfile, TARGET, TapEntry, redact_sql,
};
use crate::metrics::{CacheName, Metrics};

/// The target sqlx reports each statement on (sqlx-core's `QueryLogger`), at
/// `DEBUG`, or `WARN` past its slow-statement threshold.
const SQLX_TARGET: &str = "sqlx::query";

/// The collector. Build it with [`layer`], which also gives it its filter.
pub struct ProfilerLayer {
    store: Arc<ProfileStore>,
    metrics: Arc<Metrics>,
}

/// The profiler layer, filtered to the profiler's own spans and events and to
/// sqlx's statement events, storing into `store` and recording the tap and
/// statement histograms into `metrics`.
pub fn layer<S>(
    store: Arc<ProfileStore>,
    metrics: Arc<Metrics>,
) -> Filtered<ProfilerLayer, Targets, S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let filter = Targets::new()
        .with_target(TARGET, Level::TRACE)
        .with_target(SQLX_TARGET, Level::DEBUG);
    ProfilerLayer { store, metrics }.with_filter(filter)
}

/// What the root span of a request is building, kept in its extensions.
struct Building {
    started: Instant,
    method: String,
    route: String,
    status: u16,
    /// Each phase with its exact start, which orders the tree: two phases
    /// can start within the same rounded `start_ms`.
    phases: Vec<(Instant, PhaseEntry)>,
    query_count: u32,
    query_secs: f64,
    queries: Vec<QueryEntry>,
    taps: Vec<TapEntry>,
    hits: [u32; CacheName::ALL.len()],
    misses: [u32; CacheName::ALL.len()],
    truncated: bool,
}

/// A phase span's own record, kept in its extensions until it closes.
struct Timing {
    started: Instant,
    name: String,
    detail: String,
    plugin: String,
    tap: String,
    depth: usize,
    queries: u32,
}

/// Collects the fields the profiler's spans and the sqlx event carry. Any
/// other field is ignored, which is what keeps unknown data out.
#[derive(Default)]
struct Fields {
    method: String,
    route: String,
    status: Option<u16>,
    phase: String,
    detail: String,
    plugin: String,
    tap: String,
    cache: String,
    hit: Option<bool>,
    summary: String,
    statement: String,
    elapsed_secs: Option<f64>,
    rows: u64,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        let slot = match field.name() {
            "method" => &mut self.method,
            "route" => &mut self.route,
            "phase" => &mut self.phase,
            "detail" => &mut self.detail,
            "plugin" => &mut self.plugin,
            "tap" => &mut self.tap,
            "cache" => &mut self.cache,
            "summary" => &mut self.summary,
            "db.statement" => &mut self.statement,
            _ => return,
        };
        value.clone_into(slot);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "status" => self.status = u16::try_from(value).ok(),
            "rows_returned" => self.rows = value,
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Ok(value) = u64::try_from(value) {
            self.record_u64(field, value);
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = Some(value);
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "hit" {
            self.hit = Some(value);
        }
    }

    /// Every field this collector keeps is a string, a number or a bool; the
    /// rest (`?elapsed`, and anything a future callsite adds) is dropped.
    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

fn ms(secs: f64) -> f64 {
    (secs * 100_000.0).round() / 100.0
}

/// The request a span belongs to: the outermost profiler span above it, if
/// that is a request.
fn request_of<'a, S>(span: &SpanRef<'a, S>) -> Option<SpanRef<'a, S>>
where
    S: Subscriber + for<'l> LookupSpan<'l>,
{
    span.scope()
        .from_root()
        .next()
        .filter(|root| root.extensions().get::<Building>().is_some())
}

impl<S> Layer<S> for ProfilerLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let started = Instant::now();

        if span.parent().is_none() && attrs.metadata().name() == "request" {
            span.extensions_mut().insert(Building {
                started,
                method: fields.method,
                route: fields.route,
                status: fields.status.unwrap_or(0),
                phases: Vec::new(),
                query_count: 0,
                query_secs: 0.0,
                queries: Vec::new(),
                taps: Vec::new(),
                hits: [0; CacheName::ALL.len()],
                misses: [0; CacheName::ALL.len()],
                truncated: false,
            });
            return;
        }

        let name = match attrs.metadata().name() {
            "phase" => fields.phase,
            other => other.to_string(),
        };
        let detail = if fields.plugin.is_empty() {
            fields.detail
        } else {
            format!("{}::{}", fields.plugin, fields.tap)
        };
        span.extensions_mut().insert(Timing {
            started,
            name,
            detail,
            plugin: fields.plugin,
            tap: fields.tap,
            depth: span.scope().skip(1).count().saturating_sub(1),
            queries: 0,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = Fields::default();
        values.record(&mut fields);
        if let (Some(status), Some(building)) =
            (fields.status, span.extensions_mut().get_mut::<Building>())
        {
            building.status = status;
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let target = event.metadata().target();
        if target != SQLX_TARGET && target != TARGET {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let current = ctx.event_span(event);
        let request = current.as_ref().and_then(request_of);

        if target == SQLX_TARGET {
            let Some(secs) = fields.elapsed_secs else {
                return;
            };
            self.metrics.record_db_query(secs);
            let Some(request) = request else {
                return;
            };
            // The phase it ran in: the innermost span, unless that is the
            // request itself.
            let mut phase = String::from("request");
            if let Some(span) = current.as_ref()
                && let Some(timing) = span.extensions_mut().get_mut::<Timing>()
            {
                timing.queries += 1;
                phase.clone_from(&timing.name);
            }
            let mut ext = request.extensions_mut();
            let Some(building) = ext.get_mut::<Building>() else {
                return;
            };
            building.query_count += 1;
            building.query_secs += secs;
            if building.queries.len() >= MAX_ENTRIES {
                building.truncated = true;
                return;
            }
            // sqlx sends the full text only when it differs from the summary.
            let text = if fields.statement.trim().is_empty() {
                fields.summary
            } else {
                fields.statement
            };
            let mut statement = if self.store.show_sql_values() {
                text.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                redact_sql(&text)
            };
            if let Some((cut, _)) = statement.char_indices().nth(MAX_STATEMENT_CHARS) {
                statement.truncate(cut);
                statement.push('…');
            }
            building.queries.push(QueryEntry {
                statement,
                duration_ms: ms(secs),
                rows: fields.rows,
                phase,
            });
            return;
        }

        // A cache lookup, from `Metrics::count_cache`.
        let (Some(hit), Some(request)) = (fields.hit, request) else {
            return;
        };
        let Some(index) = CacheName::ALL
            .iter()
            .position(|c| c.as_str() == fields.cache)
        else {
            return;
        };
        if let Some(building) = request.extensions_mut().get_mut::<Building>() {
            let counts = if hit {
                &mut building.hits
            } else {
                &mut building.misses
            };
            if let Some(count) = counts.get_mut(index) {
                *count += 1;
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };

        if let Some(building) = span.extensions_mut().remove::<Building>() {
            self.store.push(finish(building));
            return;
        }

        let Some(timing) = span.extensions_mut().remove::<Timing>() else {
            return;
        };
        let secs = timing.started.elapsed().as_secs_f64();
        if timing.name == "tap" {
            self.metrics.record_tap(&timing.plugin, &timing.tap, secs);
        }
        let Some(request) = request_of(&span) else {
            return;
        };
        let mut ext = request.extensions_mut();
        let Some(building) = ext.get_mut::<Building>() else {
            return;
        };
        if building.phases.len() >= MAX_ENTRIES {
            building.truncated = true;
            return;
        }
        let start_ms = ms(timing
            .started
            .saturating_duration_since(building.started)
            .as_secs_f64());
        if timing.name == "tap" {
            if building.taps.len() < MAX_ENTRIES {
                building.taps.push(TapEntry {
                    plugin: timing.plugin.clone(),
                    tap: timing.tap.clone(),
                    duration_ms: ms(secs),
                });
            } else {
                building.truncated = true;
            }
        }
        building.phases.push((
            timing.started,
            PhaseEntry {
                name: timing.name,
                detail: timing.detail,
                depth: timing.depth,
                start_ms,
                duration_ms: ms(secs),
                queries: timing.queries,
            },
        ));
    }
}

/// A finished request's profile, numbered by the store.
fn finish(mut b: Building) -> RequestProfile {
    // Phases are recorded as they close, children first; the tree reads in
    // start order.
    // A parent starts no later than its children; on a tie, the shallower
    // first.
    b.phases
        .sort_by(|(x, px), (y, py)| x.cmp(y).then(px.depth.cmp(&py.depth)));
    b.taps
        .sort_by(|x, y| x.tap.cmp(&y.tap).then(x.plugin.cmp(&y.plugin)));
    let caches: Vec<CacheEntry> = CacheName::ALL
        .iter()
        .zip(b.hits.iter().zip(b.misses.iter()))
        .filter(|(_, (h, m))| **h > 0 || **m > 0)
        .map(|(name, (h, m))| CacheEntry {
            cache: name.as_str(),
            hits: *h,
            misses: *m,
        })
        .collect();
    RequestProfile {
        id: 0,
        at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        method: b.method,
        route: b.route,
        status: b.status,
        total_ms: ms(b.started.elapsed().as_secs_f64()),
        phases: b.phases.into_iter().map(|(_, phase)| phase).collect(),
        query_count: b.query_count,
        query_ms: ms(b.query_secs),
        queries: b.queries,
        taps: b.taps,
        cache_hits: b.hits.iter().sum(),
        cache_misses: b.misses.iter().sum(),
        caches,
        truncated: b.truncated,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::profiler::{ProfilerConfig, phase, request_span, tap_span};
    use tracing_subscriber::layer::SubscriberExt;

    fn store() -> Arc<ProfileStore> {
        ProfileStore::for_config(&ProfilerConfig {
            enabled: true,
            capture: 10,
            show_sql_values: false,
        })
        .unwrap()
    }

    /// A request's phases come out as a tree in start order, its cache
    /// lookups per cache, and its tap calls both in the profile and in the
    /// tap histogram.
    #[test]
    fn a_request_becomes_one_profile() {
        let store = store();
        let metrics = Arc::new(Metrics::new());
        let subscriber = tracing_subscriber::registry().with(layer(store.clone(), metrics.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let request = request_span("GET", "/item/{id}");
            let _r = request.enter();
            {
                let _h = phase("handler", "").entered();
                {
                    let _a = phase("access", "access content").entered();
                    let _t = tap_span("trovato_blog", "tap_item_access").entered();
                }
                metrics.count_cache(CacheName::Item, false);
                metrics.count_cache(CacheName::Item, true);
                let _p = phase("render", "page.html").entered();
            }
            crate::profiler::record_status(&request, 200);
        });

        let profiles = store.recent();
        assert_eq!(profiles.len(), 1);
        let p = &profiles[0];
        assert_eq!(
            (p.method.as_str(), p.route.as_str(), p.status),
            ("GET", "/item/{id}", 200)
        );
        let tree: Vec<(String, usize)> = p
            .phases
            .iter()
            .map(|ph| (ph.name.clone(), ph.depth))
            .collect();
        assert_eq!(
            tree,
            [
                ("handler".to_string(), 0),
                ("access".to_string(), 1),
                ("tap".to_string(), 2),
                ("render".to_string(), 1),
            ]
        );
        assert_eq!(p.taps.len(), 1);
        assert_eq!(p.taps[0].plugin, "trovato_blog");
        assert_eq!(p.caches.len(), 1);
        assert_eq!(
            (p.caches[0].cache, p.caches[0].hits, p.caches[0].misses),
            ("item", 1, 1)
        );
        assert!(metrics.encode().contains(
            "tap_duration_seconds_count{plugin=\"trovato_blog\",tap=\"tap_item_access\"} 1"
        ));
    }

    /// Spans outside a request make no profile, but a tap there is still
    /// measured.
    #[test]
    fn work_outside_a_request_is_not_a_profile() {
        let store = store();
        let metrics = Arc::new(Metrics::new());
        let subscriber = tracing_subscriber::registry().with(layer(store.clone(), metrics.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let _t = tap_span("trovato_blog", "tap_cron").entered();
            metrics.count_cache(CacheName::Item, true);
        });
        assert!(store.recent().is_empty());
        assert!(metrics.encode().contains("tap=\"tap_cron\"} 1"));
    }
}
