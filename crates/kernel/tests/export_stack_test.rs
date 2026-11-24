#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The subscriber the server runs with `TRACING_EXPORT=otlp` exports a
//! statement under its request, normalised, and nothing for a request that
//! sampling left out (BG14).
//!
//! Built with `telemetry::subscriber` and `export::provider`, as `main` builds
//! them (batch processor, parent-based ratio sampler), with an in-memory
//! exporter in place of OTLP. The statement is sqlx's own event, emitted by
//! hand inside a request profile, so it goes through the query listener as a
//! real one does and no database is needed. Fingerprint collection is off, as
//! on a server by default, so the statement's text is read only because export
//! wants it. Its own test binary, because export is installed once per process.

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::trace::{SpanContext, SpanKind, TraceContextExt as _, TraceFlags, TraceState};
use opentelemetry::trace::{SpanId, TraceId};
use opentelemetry_sdk::trace::InMemorySpanExporter;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::EnvFilter;
use trovato_kernel::system_log::SystemLogLayer;
use trovato_kernel::telemetry::export::{self, ExportSettings};
use trovato_kernel::telemetry::profile::{self, RequestProfile};
use trovato_kernel::telemetry::{TARGET, subscriber};

/// A statement as code built it with `format!`, a literal inside.
const SQL: &str = "select * from users where mail = 'alice@example.com' and id = $1";

/// What sqlx reports when a statement finishes: its event, with the fields
/// the query listener reads.
fn sqlx_reports_the_statement() {
    tracing::debug!(
        target: "sqlx::query",
        summary = "select * from users where …",
        db.statement = SQL,
        rows_returned = 1_u64,
        elapsed_secs = 0.003_f64,
    );
}

/// Run `f` as a request does: inside a request profile (no metrics, no
/// fingerprints), on a runtime of its own.
fn in_a_request(f: impl FnOnce()) {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(profile::scope(
            Arc::new(RequestProfile::new(None, false)),
            async move { f() },
        ));
}

#[test]
fn a_statement_is_exported_under_its_request_and_only_when_sampled() {
    let exporter = InMemorySpanExporter::default();
    let settings = ExportSettings {
        otlp: true,
        service_name: "trovato".into(),
        sample_ratio: 1.0,
    };
    let provider = export::provider(&settings, exporter.clone());
    let (log, _log_rows) = SystemLogLayer::new();
    let dispatch = subscriber(
        std::io::sink,
        EnvFilter::new(trovato_kernel::telemetry::DEFAULT_LOG_FILTER),
        log,
        Some(provider.clone()),
    );

    tracing::dispatcher::with_default(&dispatch, || {
        // Outside any request there is no trace to join.
        assert!(export::sampled_parent().is_none());

        // A sampled request, with a span only the log sees inside it (as
        // `tower_http`'s is): the statement still finds the request.
        let request = tracing::trace_span!(target: TARGET, "http.request", otel.name = "GET /x");
        let _in_request = request.enter();
        let log_only = tracing::info_span!("request");
        let _in_log_only = log_only.enter();
        assert!(
            export::sampled_parent().is_some(),
            "a sampled request is a parent"
        );
        in_a_request(sqlx_reports_the_statement);
    });

    tracing::dispatcher::with_default(&dispatch, || {
        // A request sampling left out (here because its parent says so; the
        // ratio sampler's own decision is in `export`'s unit tests): a
        // statement in it finds no parent, so it is not exported.
        let request = tracing::trace_span!(target: TARGET, "http.request", otel.name = "GET /y");
        let unsampled = SpanContext::new(
            TraceId::from(7_u128),
            SpanId::from(7_u64),
            TraceFlags::default(),
            true,
            TraceState::default(),
        );
        request
            .set_parent(opentelemetry::Context::new().with_remote_span_context(unsampled))
            .unwrap();
        let _in_request = request.enter();
        assert!(
            export::sampled_parent().is_none(),
            "a statement in an unsampled request found a parent"
        );
        in_a_request(sqlx_reports_the_statement);
    });

    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
    assert_eq!(names, ["db.query", "GET /x"], "{spans:#?}");
    let (query, request) = (&spans[0], &spans[1]);
    // Nothing beyond the span's own fields: no source location, thread,
    // level or target attributes.
    for kv in &request.attributes {
        let key = kv.key.as_str();
        assert!(
            !key.starts_with("code.")
                && !key.starts_with("thread.")
                && key != "level"
                && key != "target",
            "{key} was exported on the request span"
        );
    }
    assert_eq!(query.span_kind, SpanKind::Client);
    assert_eq!(query.parent_span_id, request.span_context.span_id());
    assert_eq!(
        query.span_context.trace_id(),
        request.span_context.trace_id()
    );
    let attribute = |key: &str| {
        query
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
            .unwrap_or_else(|| panic!("db.query without {key}: {query:?}"))
    };
    let text = attribute("db.query.text");
    assert!(!text.contains("alice"), "the literal was exported: {text}");
    assert!(
        !text.contains('\''),
        "a quoted literal was exported: {text}"
    );
    assert!(text.contains("mail = ?"), "{text}");
    assert!(attribute("db.query.summary").starts_with("select:users:"));
    assert_eq!(attribute("db.system.name"), "postgresql");
    assert_eq!(attribute("db.response.returned_rows"), "1");
    // Started `elapsed` before it ended.
    let took = query.end_time.duration_since(query.start_time).unwrap();
    assert!(took >= Duration::from_millis(3), "{took:?}");
}
