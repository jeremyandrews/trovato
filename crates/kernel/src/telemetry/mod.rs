//! Gander: what one request (or one queue job) did, and how long it took.
//!
//! See `docs/design/tracing-and-performance-tests.md`. Three pieces:
//!
//! - [`profile`]: per-request counters in a task-local scope, filled in by the
//!   few choke points every query, cache call, plugin call, render and
//!   outgoing request passes through.
//! - [`query_listener`]: the `tracing` layer that turns sqlx's own statement
//!   event into a counted query, so no call site is wrapped.
//! - [`fingerprint`]: a statement's name without its values.
//!
//! Spans use the tracing target [`TARGET`] at `TRACE` level. With export off
//! nothing is interested in that target, so every one of those callsites is
//! disabled once and costs an atomic load afterwards.

pub mod fingerprint;
pub mod profile;
pub mod query_listener;

pub use profile::{RequestProfile, RequestProfileSnapshot};

/// The tracing target every Gander span uses (`Design-Web-Layer.md` named it).
pub const TARGET: &str = "gander";

/// Run one template render of `kind` (`page`, `item`, `element`, `admin`)
/// inside its `template.render` span, counted into the request.
pub fn render<T>(kind: &'static str, f: impl FnOnce() -> T) -> T {
    let _span = tracing::trace_span!(
        target: TARGET,
        "template.render",
        trovato.template.kind = kind,
    )
    .entered();
    profile::render(f)
}

/// The `http.client` span of one outgoing request, counted into the request in
/// scope. Carries the method and the host only: a path or query string can
/// hold a token.
pub fn outgoing_span(method: &str, url: &str) -> tracing::Span {
    profile::count_outgoing();
    tracing::trace_span!(
        target: TARGET,
        "http.client",
        http.request.method = method,
        server.address = %server_address(url),
        http.response.status_code = tracing::field::Empty,
        gen_ai.provider.name = tracing::field::Empty,
        gen_ai.request.model = tracing::field::Empty,
    )
}

/// Send an outgoing request inside its `http.client` span, counted into the
/// request in scope: the one way the kernel sends HTTP (`host::ai` adds its
/// provider and model to the same span itself).
///
/// # Errors
///
/// Whatever building or sending the request returns.
pub async fn send(builder: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    let (client, request) = builder.build_split();
    let request = request?;
    let span = outgoing_span(request.method().as_str(), request.url().as_str());
    let response = tracing::Instrument::instrument(client.execute(request), span.clone()).await;
    if let Ok(r) = response.as_ref() {
        span.record("http.response.status_code", r.status().as_u16());
    }
    response
}

/// The span `TraceLayer` makes for each request: the method and the matched
/// route template, never the URI.
///
/// `TraceLayer::new_for_http()` records `uri=` with its query string, and the
/// default `RUST_LOG` prints that span on every log line inside a request, so
/// a search term, a reset token or an email in a URL reached the log.
pub fn http_trace_span<B>(request: &axum::http::Request<B>) -> tracing::Span {
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or(crate::middleware::query_profiler::UNMATCHED_ROUTE, |m| {
            m.as_str()
        });
    // The target `tower_http` uses for its own span, so `RUST_LOG=tower_http=debug`
    // (the default) keeps showing it exactly where it did.
    tracing::debug_span!(
        target: "tower_http::trace::make_span",
        "request",
        method = %request.method(),
        route = route,
        version = ?request.version(),
    )
}

/// The kernel's `TraceLayer`, with [`http_trace_span`] as its span.
pub fn trace_layer() -> tower_http::trace::TraceLayer<
    tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>,
    fn(&axum::http::Request<axum::body::Body>) -> tracing::Span,
> {
    tower_http::trace::TraceLayer::new_for_http().make_span_with(
        http_trace_span::<axum::body::Body>
            as fn(&axum::http::Request<axum::body::Body>) -> tracing::Span,
    )
}

/// The `cache.bin` of a Redis cache key: its first `:`-separated segment when
/// that is a plain lowercase name, otherwise `other`.
///
/// Only the first segment, and only when it is a name, because the rest of a
/// key is an id, a term or a user's value, and a span attribute must carry
/// none of those.
pub fn cache_bin(key: &str) -> &str {
    let first = key.split(':').next().unwrap_or("");
    if !first.is_empty()
        && first.len() <= 32
        && first.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        first
    } else {
        "other"
    }
}

/// The host of an outgoing URL, for `server.address`: never its path, query
/// or credentials.
pub fn server_address(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "invalid".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cache_bin_is_the_name_segment_only() {
        assert_eq!(cache_bin("search_settings"), "search_settings");
        assert_eq!(cache_bin("item:0192-abc"), "item");
        assert_eq!(cache_bin("st:0192:item:1"), "st");
        assert_eq!(cache_bin("Jane@Example.com:x"), "other");
        assert_eq!(cache_bin(""), "other");
        assert_eq!(cache_bin("t:0192"), "t");
    }

    #[test]
    fn a_server_address_drops_everything_but_the_host() {
        assert_eq!(
            server_address("https://user:secret@api.example.com/v1/x?token=abc"),
            "api.example.com"
        );
        assert_eq!(server_address("not a url"), "invalid");
    }
}
