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

    /// Cache hit counter.
    pub cache_hits: Counter,

    /// Cache miss counter.
    pub cache_misses: Counter,

    /// Active HTTP connections gauge.
    pub active_connections: Gauge,

    /// File uploads counter.
    pub file_uploads: Counter,

    /// File upload bytes counter.
    pub file_upload_bytes: Counter,

    /// Rate limit rejections counter.
    pub rate_limit_rejections: Counter,
}

impl Metrics {
    /// Create a new metrics registry.
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let http_requests = Family::<HttpLabels, Counter>::default();
        registry.register(
            "http_requests_total",
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

        let cache_hits = Counter::default();
        registry.register("cache_hits_total", "Cache hit count", cache_hits.clone());

        let cache_misses = Counter::default();
        registry.register(
            "cache_misses_total",
            "Cache miss count",
            cache_misses.clone(),
        );

        let active_connections = Gauge::default();
        registry.register(
            "http_active_connections",
            "Active HTTP connections",
            active_connections.clone(),
        );

        let file_uploads = Counter::default();
        registry.register(
            "file_uploads_total",
            "Total file uploads",
            file_uploads.clone(),
        );

        let file_upload_bytes = Counter::default();
        registry.register(
            "file_upload_bytes_total",
            "Total bytes uploaded",
            file_upload_bytes.clone(),
        );

        let rate_limit_rejections = Counter::default();
        registry.register(
            "rate_limit_rejections_total",
            "Rate limit rejections",
            rate_limit_rejections.clone(),
        );

        Self {
            registry,
            http_requests,
            http_duration_seconds,
            tap_duration_seconds,
            db_query_duration_seconds,
            cache_hits,
            cache_misses,
            active_connections,
            file_uploads,
            file_upload_bytes,
            rate_limit_rejections,
        }
    }

    /// Record an HTTP request.
    pub fn record_request(&self, method: &str, path: &str, status: u16, duration_secs: f64) {
        let labels = HttpLabels {
            method: method.to_string(),
            path: normalize_path(path),
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

    /// Record a cache hit.
    pub fn record_cache_hit(&self) {
        self.cache_hits.inc();
    }

    /// Record a cache miss.
    pub fn record_cache_miss(&self) {
        self.cache_misses.inc();
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

/// Normalize a path for metrics labels.
///
/// Replaces dynamic segments (UUIDs, IDs) with placeholders to limit cardinality.
fn normalize_path(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let normalized: Vec<String> = segments
        .into_iter()
        .map(|s| {
            // Replace UUIDs and numeric IDs with a placeholder to limit cardinality
            if uuid::Uuid::parse_str(s).is_ok()
                || (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            {
                "{id}".to_string()
            } else {
                s.to_string()
            }
        })
        .collect();
    normalized.join("/")
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("/item/123"), "/item/{id}");
        assert_eq!(
            normalize_path("/item/550e8400-e29b-41d4-a716-446655440000"),
            "/item/{id}"
        );
        assert_eq!(
            normalize_path("/admin/structure/types"),
            "/admin/structure/types"
        );
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn test_metrics_new() {
        let metrics = Metrics::new();
        let output = metrics.encode();
        assert!(output.contains("http_requests_total"));
        assert!(output.contains("cache_hits_total"));
    }

    #[test]
    fn test_record_request() {
        let metrics = Metrics::new();
        metrics.record_request("GET", "/item/123", 200, 0.05);

        let output = metrics.encode();
        assert!(output.contains("http_requests_total"));
    }
}
