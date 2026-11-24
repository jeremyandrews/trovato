//! Sending Gander spans to an OpenTelemetry backend over OTLP.
//!
//! Off unless `TRACING_EXPORT=otlp`. Off, nothing here is installed: no layer
//! is interested in the `gander` target, so every Gander span stays disabled
//! and costs one atomic load. On, the spans go through
//! `tracing-opentelemetry` and a batch processor (its own thread, so a request
//! never waits on the exporter) to the OTLP/HTTP endpoint the standard
//! `OTEL_EXPORTER_OTLP_ENDPOINT` names. See "5. Export" in
//! `docs/design/tracing-and-performance-tests.md` and the operator guide,
//! `docs/tracing.md`.
//!
//! Database statements are the one exception to "a Gander span is a
//! `tracing` span": sqlx reports a statement only when it has finished, so
//! [`db_query`] starts the OTel span after the fact, at `now - elapsed`, under
//! the request's current span.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, bail};
use opentelemetry::trace::{Span as _, SpanKind, TraceContextExt as _, Tracer as _};
use opentelemetry::{KeyValue, trace::TracerProvider as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{Sampler, SdkTracer, SdkTracerProvider, SpanExporter};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::registry::LookupSpan;

use super::{TARGET, fingerprint};

/// What `TRACING_EXPORT` and the `OTEL_*` variables ask for.
#[derive(Debug, PartialEq)]
pub struct ExportSettings {
    /// `TRACING_EXPORT=otlp`.
    pub otlp: bool,
    /// `OTEL_SERVICE_NAME`, default `trovato`.
    pub service_name: String,
    /// `OTEL_TRACES_SAMPLER_ARG`: the share of requests traced, 0 to 1,
    /// default 1. Each request is a new trace (no incoming `traceparent` is
    /// read), and the spans inside a request follow its decision.
    /// `OTEL_TRACES_SAMPLER` is not read: the sampler is always this ratio.
    pub sample_ratio: f64,
}

impl ExportSettings {
    /// Read the settings from the environment.
    ///
    /// # Errors
    ///
    /// `TRACING_EXPORT` other than `off` or `otlp`, or, with export on, a
    /// sampler ratio that is not a number from 0 to 1.
    pub fn from_env() -> Result<Self> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// [`Self::from_env`], reading through `var`.
    ///
    /// # Errors
    ///
    /// As [`Self::from_env`].
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let otlp = match var("TRACING_EXPORT").as_deref().map(str::trim) {
            None | Some("" | "off") => false,
            Some("otlp") => true,
            Some(other) => bail!("TRACING_EXPORT must be `off` or `otlp`, not `{other}`"),
        };
        let service_name = var("OTEL_SERVICE_NAME")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "trovato".to_string());
        // Read only with export on: other OpenTelemetry tooling on the host
        // may set it, and export off must not depend on it.
        let sample_ratio = match var("OTEL_TRACES_SAMPLER_ARG").filter(|_| otlp) {
            None => 1.0,
            Some(raw) => {
                let ratio: f64 = raw
                    .trim()
                    .parse()
                    .with_context(|| format!("OTEL_TRACES_SAMPLER_ARG `{raw}` is not a number"))?;
                if !(0.0..=1.0).contains(&ratio) {
                    bail!("OTEL_TRACES_SAMPLER_ARG must be from 0 to 1, not {ratio}");
                }
                ratio
            }
        };
        Ok(Self {
            otlp,
            service_name,
            sample_ratio,
        })
    }
}

/// The tracer the exporting layer and [`db_query`] share, and its provider,
/// kept to flush on shutdown: set together, once export is on.
static EXPORT: OnceLock<(SdkTracer, SdkTracerProvider)> = OnceLock::new();

/// Whether spans are being exported.
pub fn active() -> bool {
    EXPORT.get().is_some()
}

/// Build the OTLP exporter `OTEL_EXPORTER_OTLP_ENDPOINT` (default
/// `http://localhost:4318`) names, HTTP and protobuf.
///
/// # Errors
///
/// The exporter could not be built (an endpoint that is not a URL).
pub fn otlp_exporter() -> Result<opentelemetry_otlp::SpanExporter> {
    use opentelemetry_otlp::WithExportConfig as _;
    opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .build()
        .context("building the OTLP span exporter")
}

/// The tracer provider: `exporter` behind a batch processor, the sampler and
/// the service name from `settings`.
pub fn provider(
    settings: &ExportSettings,
    exporter: impl SpanExporter + 'static,
) -> SdkTracerProvider {
    SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            settings.sample_ratio,
        ))))
        .with_resource(
            Resource::builder()
                .with_service_name(settings.service_name.clone())
                .build(),
        )
        .build()
}

/// Make `provider` the one export goes through, and return the layer that
/// feeds it: Gander spans only (the `gander` target), with none of the
/// source location, thread or level attributes `tracing-opentelemetry` adds
/// by default, so what is exported is exactly the allowlist in the design.
///
/// # Panics
///
/// If called twice.
pub fn install<S>(provider: SdkTracerProvider) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let tracer = provider.tracer("trovato");
    assert!(
        EXPORT.set((tracer.clone(), provider)).is_ok(),
        "trace export is installed once"
    );
    tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_location(false)
        .with_threads(false)
        .with_level(false)
        .with_target(false)
        .with_tracked_inactivity(false)
        .with_filter(Targets::new().with_target(TARGET, tracing::Level::TRACE))
}

/// Flush and stop the exporter: the spans still in the batch are sent if the
/// collector takes them within `limit`, and dropped otherwise. Blocks; call
/// it outside the async runtime's worker threads.
pub fn shutdown(limit: Duration) {
    if let Some((_, provider)) = EXPORT.get()
        && let Err(e) = provider.shutdown_with_timeout(limit)
    {
        tracing::warn!(error = %e, "trace export did not shut down cleanly");
    }
}

/// The OTel context a statement run now would be exported under: the
/// innermost exported span the current thread is in, if its trace is
/// sampled. `None` with export off, outside any exported span (a statement
/// outside any request has no trace to join), and in a request sampling left
/// out, so such a statement is never normalised at all.
///
/// The exporting layer makes each Gander span's OTel context the thread's
/// current one while the span is entered (`tracing-opentelemetry`'s context
/// activation), and no other span touches it, so the current context is that
/// innermost exported span, even when the registry's current span is one only
/// the log sees (`tower_http`'s request span, say).
pub fn sampled_parent() -> Option<opentelemetry::Context> {
    opentelemetry::Context::map_current(|cx| cx.span().span_context().is_sampled())
        .then(opentelemetry::Context::current)
}

/// Export one finished statement, `sql` as sqlx ran it, as a `db.query` span
/// under `parent` (from [`sampled_parent`]): started `elapsed` before now,
/// ended now. The span carries the statement only normalised
/// (`db.query.text`, every literal and parameter replaced) and its
/// fingerprint (`db.query.summary`), so no literal a `format!` put into the
/// SQL reaches the backend. Does nothing with export off.
pub fn db_query(parent: &opentelemetry::Context, elapsed: Duration, sql: &str, rows: Option<u64>) {
    let Some((tracer, _)) = EXPORT.get() else {
        return;
    };
    let end = SystemTime::now();
    let start = end.checked_sub(elapsed).unwrap_or(end);
    let text = fingerprint::normalize(sql);
    let mut attributes = vec![
        KeyValue::new("db.system.name", "postgresql"),
        KeyValue::new(
            "db.query.summary",
            fingerprint::fingerprint_normalized(&text),
        ),
        KeyValue::new("db.query.text", text),
    ];
    if let Some(rows) = rows.and_then(|r| i64::try_from(r).ok()) {
        attributes.push(KeyValue::new("db.response.returned_rows", rows));
    }
    let mut span = tracer
        .span_builder("db.query")
        .with_kind(SpanKind::Client)
        .with_start_time(start)
        .with_attributes(attributes)
        .start_with_context(tracer, parent);
    span.end_with_timestamp(end);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn settings(vars: &[(&str, &str)]) -> Result<ExportSettings> {
        ExportSettings::from_vars(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        })
    }

    #[test]
    fn export_is_off_unless_asked_for() {
        let off = settings(&[]).unwrap();
        assert_eq!(
            off,
            ExportSettings {
                otlp: false,
                service_name: "trovato".into(),
                sample_ratio: 1.0,
            }
        );
        assert!(!settings(&[("TRACING_EXPORT", "off")]).unwrap().otlp);
        assert!(!settings(&[("TRACING_EXPORT", "")]).unwrap().otlp);
        assert!(settings(&[("TRACING_EXPORT", "otlp")]).unwrap().otlp);
        assert!(settings(&[("TRACING_EXPORT", " otlp ")]).unwrap().otlp);
    }

    #[test]
    fn a_setting_that_is_not_understood_stops_the_server() {
        // A typo must not quietly leave export off.
        assert!(settings(&[("TRACING_EXPORT", "jaeger")]).is_err());
        for bad in ["half", "1.5", "-0.1", "NaN", "inf"] {
            assert!(
                settings(&[("TRACING_EXPORT", "otlp"), ("OTEL_TRACES_SAMPLER_ARG", bad)]).is_err()
            );
            // With export off the ratio is not read at all.
            assert!(settings(&[("OTEL_TRACES_SAMPLER_ARG", bad)]).is_ok());
        }
    }

    #[test]
    fn the_service_name_and_ratio_are_read() {
        let s = settings(&[
            ("TRACING_EXPORT", "otlp"),
            ("OTEL_SERVICE_NAME", "blog-eu"),
            ("OTEL_TRACES_SAMPLER_ARG", "0.25"),
        ])
        .unwrap();
        assert_eq!(s.service_name, "blog-eu");
        assert!((s.sample_ratio - 0.25).abs() < f64::EPSILON);
        assert_eq!(
            settings(&[("OTEL_SERVICE_NAME", " ")])
                .unwrap()
                .service_name,
            "trovato"
        );
    }

    /// How many of 20 new traces the provider exports at `ratio`, through
    /// its batch processor into an in-memory exporter.
    fn exported_at(ratio: f64) -> usize {
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        let exporter = InMemorySpanExporter::default();
        let settings = ExportSettings {
            otlp: true,
            service_name: "blog-eu".into(),
            sample_ratio: ratio,
        };
        let provider = provider(&settings, exporter.clone());
        let tracer = provider.tracer("test");
        for _ in 0..20 {
            tracer.start("http.request").end();
        }
        provider.force_flush().unwrap();
        let exported = exporter.get_finished_spans().unwrap().len();
        provider.shutdown().unwrap();
        exported
    }

    #[test]
    fn the_provider_samples_by_the_ratio() {
        assert_eq!(exported_at(0.0), 0, "ratio 0 exported a span");
        assert_eq!(exported_at(1.0), 20, "ratio 1 must export every span");
    }

    // No lib test may install export: it is set once per process, and this
    // one relies on it being absent.
    #[test]
    fn a_statement_outside_export_is_not_exported() {
        // Export not installed in the lib tests: nothing to do, nothing panics.
        assert!(!active());
        assert!(sampled_parent().is_none());
        db_query(
            &opentelemetry::Context::new(),
            Duration::from_millis(1),
            "select 1",
            Some(1),
        );
    }
}
