//! Counting every database statement without wrapping a single call site.
//!
//! sqlx ends every statement it runs, on a pool, a transaction or a bare
//! connection, by dropping a `QueryLogger`, which emits one `tracing` event
//! with target `sqlx::query`: the SQL text (never the values bound to it), the
//! rows and the elapsed time (`sqlx-core/src/logger.rs`). This layer listens
//! for that event.
//!
//! Its filter enables the target **only while a request profile is in scope**
//! on the current task. Outside one (startup, cron, spawned work) sqlx sees no
//! one interested at its default `DEBUG` statement level and does no more work
//! than it does today; inside one it builds the event it would build with its
//! statement log turned on.

use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::layer::{Context, Filter};

use super::profile;

/// The target sqlx's statement event uses.
const SQLX_TARGET: &str = "sqlx::query";

/// The layer: add it to the subscriber with [`query_listener`].
#[derive(Debug, Default, Clone, Copy)]
pub struct QueryListener;

/// Its filter: `sqlx::query` events, while a profile is in scope.
#[derive(Debug, Default, Clone, Copy)]
pub struct WhileProfiling;

/// The listener with its filter, ready for `registry().with(..)`.
pub fn query_listener<S>() -> Filtered<QueryListener, WhileProfiling, S>
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    QueryListener.with_filter(WhileProfiling)
}

impl<S: Subscriber> Filter<S> for WhileProfiling {
    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        meta.target() == SQLX_TARGET && profile::active()
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        // Not only events: sqlx asks `tracing::enabled!` first, and that is a
        // callsite of kind `HINT` with the same target.
        if !meta.is_span() && meta.target() == SQLX_TARGET {
            Interest::sometimes()
        } else {
            Interest::never()
        }
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        // sqlx's statements log at DEBUG, slow ones at WARN.
        Some(LevelFilter::DEBUG)
    }
}

/// What the listener reads off sqlx's event.
#[derive(Default)]
struct Statement {
    summary: Option<String>,
    full: Option<String>,
    elapsed_secs: f64,
    want_text: bool,
}

impl Visit for Statement {
    fn record_str(&mut self, field: &Field, value: &str) {
        if !self.want_text {
            return;
        }
        match field.name() {
            "summary" => self.summary = Some(value.to_owned()),
            // Empty when the summary already is the whole statement.
            "db.statement" if !value.trim().is_empty() => {
                self.full = Some(value.trim().to_owned());
            }
            _ => {}
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = value;
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S: Subscriber> Layer<S> for QueryListener {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != SQLX_TARGET {
            return;
        }
        let mut statement = Statement {
            want_text: profile::wants_statements(),
            ..Statement::default()
        };
        event.record(&mut statement);
        let elapsed = Duration::try_from_secs_f64(statement.elapsed_secs).unwrap_or_default();
        let Statement { summary, full, .. } = statement;
        profile::count_query(elapsed, move || full.or(summary));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::telemetry::profile::{RequestProfile, scope};

    /// sqlx's event, as `QueryLogger::finish` emits it.
    fn emit_like_sqlx(summary: &str, statement: &str) {
        tracing::event!(
            target: "sqlx::query",
            tracing::Level::DEBUG,
            summary,
            db.statement = statement,
            rows_affected = 0u64,
            rows_returned = 1u64,
            elapsed_secs = 0.002f64,
        );
    }

    #[tokio::test]
    async fn statements_are_counted_only_inside_a_profile() {
        let subscriber = tracing_subscriber::registry().with(query_listener());
        let _guard = tracing::subscriber::set_default(subscriber);

        emit_like_sqlx("SELECT 1", "");

        let profile = Arc::new(RequestProfile::new(None, true));
        scope(profile.clone(), async {
            emit_like_sqlx("SELECT id FROM item", "");
            emit_like_sqlx(
                "SELECT id, title FROM …",
                "\n\nSELECT id, title FROM item WHERE id = $1\n",
            );
            // Not sqlx: ignored.
            tracing::debug!(summary = "SELECT 2", "something else");
        })
        .await;

        let s = profile.snapshot();
        assert_eq!(s.queries, 2);
        assert_eq!(s.query_time, Duration::from_millis(4));
        assert_eq!(s.fingerprints.len(), 2);
        assert!(s.fingerprints.iter().all(|f| f.starts_with("select:item:")));
        assert_ne!(s.fingerprints[0], s.fingerprints[1]);
    }

    /// The real thing: a statement through sqlx itself is counted, so the
    /// listener is tied to sqlx's actual event and not to a copy of it.
    #[tokio::test]
    async fn a_real_sqlx_statement_is_counted() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&trovato_test_utils::env::database_url())
            .await
            .expect("this test needs DATABASE_URL");
        let subscriber = tracing_subscriber::registry().with(query_listener());
        let _guard = tracing::subscriber::set_default(subscriber);
        let profile = Arc::new(RequestProfile::new(None, true));
        scope(profile.clone(), async {
            let _: (i32,) = sqlx::query_as("SELECT 1 + $1")
                .bind(1i32)
                .fetch_one(&pool)
                .await
                .unwrap();
        })
        .await;
        let s = profile.snapshot();
        assert_eq!(s.queries, 1, "{s:?}");
        assert_eq!(
            s.fingerprints,
            vec![crate::telemetry::fingerprint::fingerprint("SELECT 1 + $1")]
        );
    }
}
