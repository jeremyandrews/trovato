//! Prometheus metrics collection.
//!
//! Provides application metrics in Prometheus format.

use prometheus_client::encoding::{EncodeLabelSet, text::encode};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;

/// HTTP request labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpLabels {
    pub method: String,
    pub path: String,
    pub status: u16,
}

/// Cache labels: which kernel cache a hit or miss belongs to.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CacheLabels {
    /// One of the [`CacheName`] label values.
    pub cache: &'static str,
}

/// Every cache the kernel counts hits and misses for.
///
/// A closed set, so the `cache` label on `cache_hits_total` and
/// `cache_misses_total` has a fixed, small cardinality: one series per name,
/// created at startup.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum CacheName {
    /// `ItemService`: items by id.
    Item,
    /// `ItemService`: field-access decisions from `tap_field_access`.
    FieldAccess,
    /// `UserService`: users by id.
    User,
    /// `PermissionService`: a user's roles and permissions.
    Permissions,
    /// `ContentTypeRegistry`: content type definitions.
    ContentType,
    /// `GatherService`: registered gather queries.
    GatherQuery,
    /// `GatherService`: distinct values of a field, for exposed filters.
    GatherDistinct,
    /// `CategoryService`: categories by id.
    Category,
    /// `CategoryService`: the tags of a category.
    CategoryTags,
    /// The config menus of a stage and language.
    Menu,
    /// `LocaleService`: translated strings.
    Locale,
    /// `ThemeEngine`: the template a suggestion list resolves to.
    TemplateSuggestion,
    /// `CacheLayer`, in Redis. The label keeps its `layer_l2` name from when
    /// the layer also had an in-process tier, so existing dashboards still read.
    LayerL2,
}

impl CacheName {
    /// Every cache name, in label order.
    ///
    /// In declaration order: a name's discriminant is its index here.
    pub const ALL: [CacheName; 13] = [
        CacheName::Item,
        CacheName::FieldAccess,
        CacheName::User,
        CacheName::Permissions,
        CacheName::ContentType,
        CacheName::GatherQuery,
        CacheName::GatherDistinct,
        CacheName::Category,
        CacheName::CategoryTags,
        CacheName::Menu,
        CacheName::Locale,
        CacheName::TemplateSuggestion,
        CacheName::LayerL2,
    ];

    /// The `cache` label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            CacheName::Item => "item",
            CacheName::FieldAccess => "field_access",
            CacheName::User => "user",
            CacheName::Permissions => "permissions",
            CacheName::ContentType => "content_type",
            CacheName::GatherQuery => "gather_query",
            CacheName::GatherDistinct => "gather_distinct",
            CacheName::Category => "category",
            CacheName::CategoryTags => "category_tags",
            CacheName::Menu => "menu",
            CacheName::Locale => "locale",
            CacheName::TemplateSuggestion => "template_suggestion",
            CacheName::LayerL2 => "layer_l2",
        }
    }
}

/// Route labels: the matched route template only.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RouteLabels {
    pub route: String,
}

/// TAP invocation labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TapLabels {
    pub plugin: String,
    pub tap: String,
}

/// Application metrics.
pub struct Metrics {
    registry: Registry,

    /// HTTP request counter by method/path/status.
    pub http_requests: Family<HttpLabels, Counter>,

    /// HTTP request duration histogram.
    pub http_duration_seconds: Family<HttpLabels, Histogram>,

    /// WASM TAP invocation duration.
    pub tap_duration_seconds: Family<TapLabels, Histogram>,

    /// Database query duration.
    pub db_query_duration_seconds: Histogram,

    /// Cache hits by cache name.
    pub cache_hits: Family<CacheLabels, Counter>,

    /// Cache misses by cache name.
    pub cache_misses: Family<CacheLabels, Counter>,

    /// Values written to the Redis cache, by cache name.
    pub cache_sets: Family<CacheLabels, Counter>,

    /// Database statements per request, by route template.
    pub http_request_db_queries: Family<RouteLabels, Histogram>,

    /// The `cache_hits` series, one per [`CacheName`], indexed by its
    /// position in [`CacheName::ALL`]: counting a lookup is one atomic add,
    /// with no label hashing or family lock on the read path.
    hit_series: [Counter; CacheName::ALL.len()],

    /// The `cache_misses` series, indexed like `hit_series`.
    miss_series: [Counter; CacheName::ALL.len()],

    /// Active HTTP connections gauge.
    pub active_connections: Gauge,

    /// Current database connection pool size.
    pub db_pool_size: Gauge,

    /// Idle database connections.
    pub db_pool_idle: Gauge,

    /// Active database connections.
    pub db_pool_active: Gauge,

    /// Maximum database connection pool size.
    pub db_pool_max: Gauge,

    /// File uploads counter.
    pub file_uploads: Counter,

    /// File upload bytes counter.
    pub file_upload_bytes: Counter,

    /// Rate limit rejections counter.
    pub rate_limit_rejections: Counter,

    /// Permission loads that failed and degraded the request to the deny-all
    /// permission set.
    ///
    /// The signature of the front-page bug this counter exists to make visible
    /// is content silently vanishing for every visitor: an empty permission set
    /// looks exactly like a permission model that is working. Any non-zero value
    /// here means at least one request was answered with permissions the viewer
    /// did not actually have (none), so it should alert.
    pub permission_load_failures: Counter,

    /// AI provider circuit breaker state (0=closed, 1=open, 2=half_open).
    pub ai_circuit_breaker_state: Gauge,

    /// Email SMTP circuit breaker state (0=closed, 1=open, 2=half_open).
    pub email_circuit_breaker_state: Gauge,
}

impl Metrics {
    /// Create a new metrics registry.
    ///
    /// Counters are registered without their `_total` suffix: the encoder
    /// appends it, so `cache_hits` is exposed as `cache_hits_total`. They used
    /// to be registered with it, and every counter was exposed with the suffix
    /// twice (`cache_hits_total_total`), under a name no documentation gave.
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let http_requests = Family::<HttpLabels, Counter>::default();
        registry.register(
            "http_requests",
            "Total HTTP requests",
            http_requests.clone(),
        );

        let http_duration_seconds = Family::<HttpLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(0.001, 2.0, 12))
        });
        registry.register(
            "http_request_duration_seconds",
            "HTTP request duration in seconds",
            http_duration_seconds.clone(),
        );

        let tap_duration_seconds = Family::<TapLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(0.0001, 2.0, 14))
        });
        registry.register(
            "tap_duration_seconds",
            "TAP invocation duration in seconds",
            tap_duration_seconds.clone(),
        );

        let db_query_duration_seconds = Histogram::new(exponential_buckets(0.0001, 2.0, 14));
        registry.register(
            "db_query_duration_seconds",
            "Database query duration in seconds",
            db_query_duration_seconds.clone(),
        );

        let cache_hits = Family::<CacheLabels, Counter>::default();
        registry.register(
            "cache_hits",
            "Cache hit count, by cache",
            cache_hits.clone(),
        );

        let cache_misses = Family::<CacheLabels, Counter>::default();
        registry.register(
            "cache_misses",
            "Cache miss count, by cache",
            cache_misses.clone(),
        );

        let cache_sets = Family::<CacheLabels, Counter>::default();
        registry.register(
            "cache_sets",
            "Values written to the Redis cache, by cache",
            cache_sets.clone(),
        );

        let http_request_db_queries =
            Family::<RouteLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(exponential_buckets(1.0, 2.0, 10))
            });
        registry.register(
            "http_request_db_queries",
            "Database statements per request, by route template",
            http_request_db_queries.clone(),
        );

        let series = |family: &Family<CacheLabels, Counter>| {
            CacheName::ALL.map(|name| {
                family
                    .get_or_create(&CacheLabels {
                        cache: name.as_str(),
                    })
                    .clone()
            })
        };
        let hit_series = series(&cache_hits);
        let miss_series = series(&cache_misses);

        let active_connections = Gauge::default();
        registry.register(
            "http_active_connections",
            "Active HTTP connections",
            active_connections.clone(),
        );

        let db_pool_size = Gauge::default();
        registry.register(
            "trovato_db_pool_size",
            "Current database connection pool size",
            db_pool_size.clone(),
        );

        let db_pool_idle = Gauge::default();
        registry.register(
            "trovato_db_pool_idle",
            "Idle database connections",
            db_pool_idle.clone(),
        );

        let db_pool_active = Gauge::default();
        registry.register(
            "trovato_db_pool_active",
            "Active database connections",
            db_pool_active.clone(),
        );

        let db_pool_max = Gauge::default();
        registry.register(
            "trovato_db_pool_max",
            "Maximum database connection pool size",
            db_pool_max.clone(),
        );

        let file_uploads = Counter::default();
        registry.register("file_uploads", "Total file uploads", file_uploads.clone());

        let file_upload_bytes = Counter::default();
        registry.register(
            "file_upload_bytes",
            "Total bytes uploaded",
            file_upload_bytes.clone(),
        );

        let rate_limit_rejections = Counter::default();
        registry.register(
            "rate_limit_rejections",
            "Rate limit rejections",
            rate_limit_rejections.clone(),
        );

        let permission_load_failures = Counter::default();
        registry.register(
            "trovato_permission_load_failures",
            "Permission loads that failed and degraded a request to the deny-all permission set",
            permission_load_failures.clone(),
        );

        let ai_circuit_breaker_state = Gauge::default();
        registry.register(
            "trovato_circuit_breaker_ai",
            "AI provider circuit breaker state (0=closed, 1=open, 2=half_open)",
            ai_circuit_breaker_state.clone(),
        );

        let email_circuit_breaker_state = Gauge::default();
        registry.register(
            "trovato_circuit_breaker_email",
            "Email SMTP circuit breaker state (0=closed, 1=open, 2=half_open)",
            email_circuit_breaker_state.clone(),
        );

        Self {
            registry,
            http_requests,
            http_duration_seconds,
            tap_duration_seconds,
            db_query_duration_seconds,
            cache_hits,
            cache_misses,
            cache_sets,
            http_request_db_queries,
            hit_series,
            miss_series,
            active_connections,
            db_pool_size,
            db_pool_idle,
            db_pool_active,
            db_pool_max,
            file_uploads,
            file_upload_bytes,
            rate_limit_rejections,
            permission_load_failures,
            ai_circuit_breaker_state,
            email_circuit_breaker_state,
        }
    }

    /// Record an HTTP request.
    ///
    /// `route` is the matched route template (`/item/{id}`), never the raw
    /// request path, so the label's cardinality is the number of routes.
    pub fn record_request(&self, method: &str, route: &str, status: u16, duration_secs: f64) {
        let labels = HttpLabels {
            method: method.to_string(),
            path: route.to_string(),
            status,
        };

        self.http_requests.get_or_create(&labels).inc();
        self.http_duration_seconds
            .get_or_create(&labels)
            .observe(duration_secs);
    }

    /// Record a TAP invocation.
    pub fn record_tap(&self, plugin: &str, tap_name: &str, duration_secs: f64) {
        let labels = TapLabels {
            plugin: plugin.to_string(),
            tap: tap_name.to_string(),
        };

        self.tap_duration_seconds
            .get_or_create(&labels)
            .observe(duration_secs);
    }

    /// Record a database query.
    pub fn record_db_query(&self, duration_secs: f64) {
        self.db_query_duration_seconds.observe(duration_secs);
    }

    /// Count one lookup in a kernel cache and pass its result through.
    ///
    /// The one place a cache read is counted: every read site wraps its
    /// lookup, `metrics.cache_lookup(CacheName::Item, cache.get(&id))`, and
    /// gets the lookup back unchanged. `Some` is a hit, `None` a miss.
    pub fn cache_lookup<T>(&self, cache: CacheName, lookup: Option<T>) -> Option<T> {
        self.count_cache(cache, lookup.is_some());
        lookup
    }

    /// Count the statements one request made, under its route template.
    pub fn record_request_queries(&self, route: &str, queries: u32) {
        self.http_request_db_queries
            .get_or_create(&RouteLabels {
                route: route.to_string(),
            })
            .observe(f64::from(queries));
    }

    /// Count one value written to the Redis cache.
    pub fn count_cache_set(&self, cache: CacheName) {
        self.cache_sets
            .get_or_create(&CacheLabels {
                cache: cache.as_str(),
            })
            .inc();
        crate::telemetry::profile::count_cache_set();
    }

    /// Count a hit (`hit = true`) or a miss in `cache`.
    ///
    /// For a read whose result is not an `Option`; prefer
    /// [`Metrics::cache_lookup`] where it is.
    pub fn count_cache(&self, cache: CacheName, hit: bool) {
        let series = if hit {
            &self.hit_series
        } else {
            &self.miss_series
        };
        // `ALL` lists every variant in declaration order, so the discriminant
        // is the index (pinned by `cache_names_index_their_own_series`).
        if let Some(counter) = series.get(cache as usize) {
            counter.inc();
        }
        // The same choke point counts the lookup into the request in scope
        // and, for an in-process cache, marks it on the enclosing span (an
        // event, not a span: a lookup takes well under a microsecond).
        crate::telemetry::profile::count_cache_get(hit);
        if cache != CacheName::LayerL2 {
            let bin = cache.as_str();
            tracing::event!(
                target: crate::telemetry::TARGET,
                tracing::Level::TRACE,
                cache.tier = "memory",
                cache.bin = bin,
                cache.hit = hit,
                "cache.get"
            );
        }
    }

    /// Record a file upload.
    pub fn record_upload(&self, bytes: u64) {
        self.file_uploads.inc();
        self.file_upload_bytes.inc_by(bytes);
    }

    /// Record a rate limit rejection.
    pub fn record_rate_limit(&self) {
        self.rate_limit_rejections.inc();
    }

    /// Increment active connections.
    pub fn connection_start(&self) {
        self.active_connections.inc();
    }

    /// Decrement active connections.
    pub fn connection_end(&self) {
        self.active_connections.dec();
    }

    /// Encode metrics in Prometheus text format.
    ///
    /// # Panics
    ///
    /// Panics if Prometheus metric encoding to a `String` buffer fails.
    /// The `fmt::Write` impl for `String` is infallible, and all metric
    /// labels use derived `Display`/`EncodeLabelSet` impls that do not
    /// produce `fmt::Error`.
    pub fn encode(&self) -> String {
        let mut buffer = String::new();
        // Prometheus encoding to String buffer is infallible
        #[allow(clippy::expect_used)]
        encode(&mut buffer, &self.registry).expect("encoding metrics");
        buffer
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metrics").finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_new() {
        let metrics = Metrics::new();
        let output = metrics.encode();
        // An empty family has no samples yet, only its declaration.
        assert!(output.contains("# TYPE http_requests counter"));
        assert!(output.contains("cache_hits_total"));
    }

    /// Every counter is exposed under its documented name, with `_total`
    /// once.
    #[test]
    fn counters_carry_the_total_suffix_once() {
        let metrics = Metrics::new();
        metrics.record_request("GET", "/", 200, 0.01);
        let output = metrics.encode();
        assert!(!output.contains("_total_total"), "{output}");
        for name in [
            "http_requests_total",
            "cache_hits_total",
            "cache_misses_total",
            "file_uploads_total",
            "file_upload_bytes_total",
            "rate_limit_rejections_total",
            "trovato_permission_load_failures_total",
        ] {
            assert!(output.contains(name), "{name} is missing from {output}");
        }
    }

    #[test]
    fn test_record_request() {
        let metrics = Metrics::new();
        metrics.record_request("GET", "/item/{id}", 200, 0.05);

        let output = metrics.encode();
        assert!(output.contains("http_requests_total"));
    }

    /// `count_cache` indexes the series by discriminant, which is only right
    /// while `ALL` lists the variants in declaration order.
    #[test]
    fn cache_names_index_their_own_series() {
        for (i, name) in CacheName::ALL.iter().enumerate() {
            assert_eq!(*name as usize, i, "{name:?} is out of order in ALL");
        }
    }

    /// Every cache has its own pair of series from startup, and a lookup
    /// counts against its own name only.
    #[test]
    fn cache_lookups_count_per_cache() {
        let metrics = Metrics::new();
        assert_eq!(metrics.cache_lookup(CacheName::Item, Some(7)), Some(7));
        assert_eq!(metrics.cache_lookup::<u8>(CacheName::Item, None), None);
        metrics.count_cache(CacheName::Item, true);

        let output = metrics.encode();
        assert!(
            output.contains("cache_hits_total{cache=\"item\"} 2"),
            "{output}"
        );
        assert!(
            output.contains("cache_misses_total{cache=\"item\"} 1"),
            "{output}"
        );
        assert!(
            output.contains("cache_hits_total{cache=\"layer_l2\"} 0"),
            "{output}"
        );
    }
}
