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
use tracing::dispatcher::WeakDispatch;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::registry::LookupSpan;

use super::TARGET;

/// What `TRACING_EXPORT` and the `OTEL_*` variables ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    /// `TRACING_EXPORT=otlp`.
    pub otlp: bool,
    /// `OTEL_SERVICE_NAME`, default `trovato`.
    pub service_name: String,
    /// `OTEL_TRACES_SAMPLER_ARG`: the share of new traces kept, 0 to 1,
    /// default 1. A request whose caller sent a sampled trace context is
    /// kept whatever this says (parent based).
    pub sample_ratio: f64,
}

impl ExportSettings {
    /// Read the settings from the environment.
    ///
    /// # Errors
    ///
    /// `TRACING_EXPORT` other than `off` or `otlp`, or a sampler ratio that
    /// is not a number from 0 to 1.
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
        let sample_ratio = match var("OTEL_TRACES_SAMPLER_ARG") {
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

/// The tracer the exporting layer and [`db_query`] share, once export is on.
static TRACER: OnceLock<SdkTracer> = OnceLock::new();
/// Its provider, kept to flush on shutdown.
static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
/// The global dispatcher, for finding a statement's parent span (see
/// [`db_query`]).
static DISPATCH: OnceLock<WeakDispatch> = OnceLock::new();

/// Whether spans are being exported.
pub fn active() -> bool {
    TRACER.get().is_some()
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
        TRACER.set(tracer.clone()).is_ok() && PROVIDER.set(provider).is_ok(),
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

/// Remember the global dispatcher, so [`db_query`] can find a statement's
/// parent without asking for the current dispatcher, which a layer cannot do
/// from inside one of its own callbacks.
pub fn set_dispatch(dispatch: &tracing::Dispatch) {
    let _ = DISPATCH.set(dispatch.downgrade());
}

/// Flush and stop the exporter: the spans still in the batch are sent.
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get()
        && let Err(e) = provider.shutdown()
    {
        tracing::warn!(error = %e, "trace export did not shut down cleanly");
    }
}

/// Export one finished statement as a `db.query` span under the current
/// request's span: started `elapsed` before now, ended now. `summary` is the
/// fingerprint and `text` the normalised SQL, literals already replaced.
///
/// Does nothing unless export is on and the statement ran inside an exported
/// span (a statement outside any request has no trace to join).
pub fn db_query(elapsed: Duration, summary: &str, text: &str, rows: Option<u64>) {
    let (Some(tracer), Some(dispatch)) =
        (TRACER.get(), DISPATCH.get().and_then(WeakDispatch::upgrade))
    else {
        return;
    };
    let Some(parent) = current_otel_context(&dispatch) else {
        return;
    };
    let end = SystemTime::now();
    let start = end.checked_sub(elapsed).unwrap_or(end);
    let mut attributes = vec![
        KeyValue::new("db.system.name", "postgresql"),
        KeyValue::new("db.query.summary", summary.to_string()),
        KeyValue::new("db.query.text", text.to_string()),
    ];
    if let Some(rows) = rows.and_then(|r| i64::try_from(r).ok()) {
        attributes.push(KeyValue::new("db.response.returned_rows", rows));
    }
    let mut span = tracer
        .span_builder("db.query")
        .with_kind(SpanKind::Client)
        .with_start_time(start)
        .with_attributes(attributes)
        .start_with_context(tracer, &parent);
    span.end_with_timestamp(end);
}

/// The OTel context of the innermost exported span the current thread is in.
/// The registry's current span may be one only the log sees (`tower_http`'s
/// request span, say), so the scope is walked outwards to the first span the
/// exporting layer knows.
fn current_otel_context(dispatch: &tracing::Dispatch) -> Option<opentelemetry::Context> {
    let current = dispatch.current_span();
    let id = current.id()?;
    let registry = dispatch.downcast_ref::<tracing_subscriber::Registry>()?;
    let scope: Vec<tracing::span::Id> = registry.span(id)?.scope().map(|span| span.id()).collect();
    scope.iter().find_map(|id| {
        tracing_opentelemetry::get_otel_context(id, dispatch)
            .filter(|cx| cx.span().span_context().is_valid())
    })
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
    }

    #[test]
    fn a_setting_that_is_not_understood_stops_the_server() {
        // A typo must not quietly leave export off.
        assert!(settings(&[("TRACING_EXPORT", "jaeger")]).is_err());
        assert!(settings(&[("OTEL_TRACES_SAMPLER_ARG", "half")]).is_err());
        assert!(settings(&[("OTEL_TRACES_SAMPLER_ARG", "1.5")]).is_err());
        assert!(settings(&[("OTEL_TRACES_SAMPLER_ARG", "-0.1")]).is_err());
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

    #[test]
    fn a_statement_outside_export_is_not_exported() {
        // Export not installed in the lib tests: nothing to do, nothing panics.
        assert!(!active());
        db_query(
            Duration::from_millis(1),
            "select:item:00000000",
            "select ?",
            Some(1),
        );
    }
}
