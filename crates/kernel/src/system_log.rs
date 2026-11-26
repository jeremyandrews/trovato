//! The site log: warnings, errors and a few notable events, kept in the
//! `system_log` table for `/admin/reports/log` (B107).
//!
//! Application errors were visible only in the container's output, which an
//! administrator of a hosted site may never see. [`SystemLogLayer`] is a
//! `tracing` layer that turns each `warn` and `error` event, and each `info`
//! event naming an allowlisted `log_type` (a sign in, a plugin enabled or
//! disabled, a config import), into a row. It never blocks the code that logs:
//! it hands the row to a bounded channel and drops it if the channel is full.
//! [`spawn_writer`] drains the channel into the table in batches once the
//! database pool exists.
//!
//! **What is never stored.** A field whose name says it holds a secret (a
//! password, token, secret, key, cookie, session, authorization header or a
//! request body) is dropped before the row is built, whatever its value, and
//! every value is truncated. Code that logs is still responsible for not
//! putting a secret in a message: this is a second line, not the first.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;
use sqlx::{AssertSqlSafe, PgPool};
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// `log_type` values an `info` event may carry to be kept. Anything else at
/// `info` or below is left to the process output.
pub const INFO_ALLOWLIST: &[&str] = &["user", "plugin", "config"];

/// Rows waiting for the writer before the layer starts dropping them.
const CHANNEL_CAPACITY: usize = 2048;

/// Rows the writer inserts in one statement.
const BATCH: usize = 200;

/// The longest message kept, in characters.
const MAX_MESSAGE: usize = 2000;

/// The longest field value kept, in characters.
const MAX_VALUE: usize = 500;

/// Default retention window, in days (`SYSTEM_LOG_RETENTION_DAYS`).
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

/// Default row cap (`SYSTEM_LOG_MAX_ROWS`).
pub const DEFAULT_MAX_ROWS: i64 = 100_000;

/// Parts of a field name that mark it as one never to store.
const SECRET_NAMES: &[&str] = &[
    "pass",
    "secret",
    "token",
    "key",
    "cookie",
    "session",
    "authorization",
    "auth_header",
    "body",
    "csrf",
    "jwt",
    "credential",
];

/// Targets whose events are never stored: the writer's own database driver,
/// so a failing insert cannot log itself in a loop. The writer's own report
/// of a failure is at `debug`, which is never stored either.
const SKIPPED_TARGETS: &[&str] = &["sqlx"];

/// One row, as the layer builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    pub created: i64,
    pub level: &'static str,
    pub log_type: String,
    pub message: String,
    pub context: BTreeMap<String, String>,
}

/// Whether a field called `name` may be stored.
pub fn is_storable_field(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !SECRET_NAMES.iter().any(|s| lower.contains(s))
}

fn truncate(mut s: String, max: usize) -> String {
    if let Some((at, _)) = s.char_indices().nth(max) {
        s.truncate(at);
        s.push('…');
    }
    s
}

#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    log_type: Option<String>,
    fields: BTreeMap<String, String>,
}

impl FieldVisitor {
    fn record(&mut self, field: &Field, value: String) {
        match field.name() {
            "message" => self.message = Some(value),
            "log_type" => self.log_type = Some(value),
            name if is_storable_field(name) => {
                self.fields
                    .insert(name.to_string(), truncate(value, MAX_VALUE));
            }
            _ => {}
        }
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut s = String::new();
        let _ = write!(s, "{value:?}");
        self.record(field, s);
    }
}

/// Turn an event into a row, or `None` if it is not one the log keeps.
pub fn record_for(event: &Event<'_>) -> Option<LogRecord> {
    let meta = event.metadata();
    let target = meta.target();
    if SKIPPED_TARGETS
        .iter()
        .any(|t| target == *t || target.starts_with(&format!("{t}::")))
    {
        return None;
    }
    let level = match *meta.level() {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        _ => return None,
    };

    let mut visitor = FieldVisitor::default();
    event.record(&mut visitor);

    if level == "info"
        && !visitor
            .log_type
            .as_deref()
            .is_some_and(|t| INFO_ALLOWLIST.contains(&t))
    {
        return None;
    }

    let log_type = visitor.log_type.unwrap_or_else(|| {
        // The library and the server binary compile the same modules under
        // two crate names.
        target
            .strip_prefix("trovato_kernel::")
            .or_else(|| target.strip_prefix("trovato::"))
            .unwrap_or(target)
            .to_string()
    });
    Some(LogRecord {
        created: chrono::Utc::now().timestamp(),
        level,
        log_type: truncate(log_type, 120),
        message: truncate(visitor.message.unwrap_or_default(), MAX_MESSAGE),
        context: visitor.fields,
    })
}

/// The `tracing` layer. Cheap to clone; every clone feeds the same writer.
#[derive(Clone)]
pub struct SystemLogLayer {
    sender: mpsc::Sender<LogRecord>,
}

/// The receiving end, handed to [`spawn_writer`].
pub struct SystemLogReceiver(mpsc::Receiver<LogRecord>);

impl SystemLogLayer {
    /// A layer and the receiver its rows arrive on.
    pub fn new() -> (Self, SystemLogReceiver) {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        (Self { sender }, SystemLogReceiver(receiver))
    }
}

impl<S: Subscriber> Layer<S> for SystemLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if let Some(record) = record_for(event) {
            // Never wait: a full channel means the writer is behind or gone,
            // and the code that logged must not be held up by its log.
            let _ = self.sender.try_send(record);
        }
    }
}

/// Insert `records` in one statement.
pub async fn insert(pool: &PgPool, records: &[LogRecord]) -> Result<(), sqlx::Error> {
    if records.is_empty() {
        return Ok(());
    }
    let mut created = Vec::with_capacity(records.len());
    let mut levels = Vec::with_capacity(records.len());
    let mut types = Vec::with_capacity(records.len());
    let mut messages = Vec::with_capacity(records.len());
    let mut contexts = Vec::with_capacity(records.len());
    for r in records {
        created.push(r.created);
        levels.push(r.level.to_string());
        types.push(r.log_type.clone());
        messages.push(r.message.clone());
        contexts.push(serde_json::to_value(&r.context).unwrap_or_default());
    }
    sqlx::query(
        "INSERT INTO system_log (created, level, log_type, message, context) \
         SELECT * FROM UNNEST($1::bigint[], $2::varchar[], $3::varchar[], $4::text[], $5::jsonb[])",
    )
    .bind(&created)
    .bind(&levels)
    .bind(&types)
    .bind(&messages)
    .bind(&contexts)
    .execute(pool)
    .await?;
    Ok(())
}

/// Write one `info` entry straight to the table, for a process that has no
/// [`SystemLogLayer`]: the `trovato plugin` CLI. Best effort; a failure is
/// only reported on stderr, since the command itself succeeded.
pub async fn record_info(pool: &PgPool, log_type: &str, message: &str, context: &[(&str, &str)]) {
    let record = LogRecord {
        created: chrono::Utc::now().timestamp(),
        level: "info",
        log_type: log_type.to_string(),
        message: message.to_string(),
        context: context
            .iter()
            .filter(|(k, _)| is_storable_field(k))
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    };
    if let Err(e) = insert(pool, &[record]).await {
        eprintln!("warning: could not write to the site log: {e}");
    }
}

/// Drain `receiver` into `system_log` until every sender is gone.
pub fn spawn_writer(pool: PgPool, receiver: SystemLogReceiver) -> tokio::task::JoinHandle<()> {
    let SystemLogReceiver(mut receiver) = receiver;
    tokio::spawn(async move {
        let mut batch = Vec::with_capacity(BATCH);
        while receiver.recv_many(&mut batch, BATCH).await > 0 {
            if let Err(e) = insert(&pool, &batch).await {
                // At `debug`, which is never stored, so a failing insert
                // cannot feed itself.
                tracing::debug!(error = %e, rows = batch.len(), "system log write failed");
            }
            batch.clear();
        }
    })
}

/// Delete rows older than `days`, then all but the newest `max_rows`: the log
/// keeps whichever is smaller. Returns how many rows went.
pub async fn prune(pool: &PgPool, days: i64, max_rows: i64) -> Result<u64, sqlx::Error> {
    let cutoff = chrono::Utc::now().timestamp() - days.max(0) * 86_400;
    let by_age = sqlx::query("DELETE FROM system_log WHERE created < $1")
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected();
    let by_count = sqlx::query(
        "DELETE FROM system_log WHERE id < \
         (SELECT id FROM system_log ORDER BY id DESC OFFSET $1 LIMIT 1)",
    )
    .bind(max_rows.max(0).saturating_sub(1))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(by_age + by_count)
}

/// A row as the log screen shows it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct LogEntry {
    pub id: i64,
    pub created: i64,
    pub level: String,
    pub log_type: String,
    pub message: String,
    pub context: serde_json::Value,
}

/// What the log screen asks for.
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub level: Option<String>,
    pub log_type: Option<String>,
    /// An escaped `ILIKE` pattern the message must match.
    pub message_like: Option<String>,
}

/// The conditions a [`LogFilter`] adds, its values bound as `$1` to `$3`.
const LOG_FILTER_SQL: &str = "WHERE ($1::text IS NULL OR level = $1) \
     AND ($2::text IS NULL OR log_type = $2) \
     AND ($3::text IS NULL OR message ILIKE $3)";

/// One page of entries and the total that match, ordered newest first, or
/// by `order_by` when given. `order_by` must come from the screen's own
/// allowed columns (`routes::admin_list::Sort::order_by`), never from a
/// request.
pub async fn list(
    pool: &PgPool,
    filter: &LogFilter,
    order_by: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<LogEntry>, i64), sqlx::Error> {
    let order_by = order_by.unwrap_or("id DESC");
    let list_sql = format!(
        "SELECT id, created, level, log_type, message, context FROM system_log \
         {LOG_FILTER_SQL} ORDER BY {order_by}, id DESC LIMIT $4 OFFSET $5"
    );
    let entries = sqlx::query_as::<_, LogEntry>(AssertSqlSafe(list_sql))
        .bind(&filter.level)
        .bind(&filter.log_type)
        .bind(&filter.message_like)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let count_sql = format!("SELECT count(*) FROM system_log {LOG_FILTER_SQL}");
    let total: i64 = sqlx::query_scalar(AssertSqlSafe(count_sql))
        .bind(&filter.level)
        .bind(&filter.log_type)
        .bind(&filter.message_like)
        .fetch_one(pool)
        .await?;
    Ok((entries, total))
}

/// Every `log_type` in the log, for the filter.
pub async fn types(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT DISTINCT log_type FROM system_log ORDER BY log_type")
        .fetch_all(pool)
        .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    /// A layer that keeps what `record_for` returns, for these tests.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<LogRecord>>>);

    impl<S: Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            if let Some(r) = record_for(event) {
                self.0.lock().unwrap().push(r);
            }
        }
    }

    fn capture(f: impl FnOnce()) -> Vec<LogRecord> {
        let cap = Capture::default();
        let subscriber = tracing_subscriber::registry().with(cap.clone());
        tracing::subscriber::with_default(subscriber, f);
        cap.0.lock().unwrap().clone()
    }

    #[test]
    fn warn_and_error_are_kept_and_plain_info_is_not() {
        let rows = capture(|| {
            tracing::info!("routine");
            tracing::debug!("noise");
            tracing::warn!(item = 3, "something odd");
            tracing::error!("broken");
        });
        assert_eq!(
            rows.iter().map(|r| r.level).collect::<Vec<_>>(),
            vec!["warn", "error"]
        );
        assert_eq!(rows[0].message, "something odd");
        assert_eq!(rows[0].context.get("item").map(String::as_str), Some("3"));
    }

    #[test]
    fn the_type_is_the_module_under_either_crate_name() {
        let rows = capture(|| {
            tracing::warn!(target: "trovato::middleware::query_profiler", "slow");
            tracing::warn!(target: "trovato_kernel::cron", "late");
            tracing::warn!(target: "hyper::proto", "odd");
        });
        assert_eq!(
            rows.iter().map(|r| r.log_type.as_str()).collect::<Vec<_>>(),
            vec!["middleware::query_profiler", "cron", "hyper::proto"]
        );
    }

    #[test]
    fn an_allowlisted_info_event_is_kept_under_its_type() {
        let rows = capture(|| {
            tracing::info!(log_type = "user", username = "maria", "login failed");
            tracing::info!(log_type = "chatter", "not on the list");
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].log_type, "user");
        assert_eq!(
            rows[0].context.get("username").map(String::as_str),
            Some("maria")
        );
    }

    #[test]
    fn secret_fields_are_never_kept() {
        let rows = capture(|| {
            tracing::warn!(
                username = "maria",
                password = "hunter2",
                api_token = "t0k",
                session_id = "s1",
                request_body = "{}",
                cookie = "c",
                "login trouble"
            );
        });
        let context = &rows[0].context;
        assert_eq!(context.keys().collect::<Vec<_>>(), vec!["username"]);
        assert!(!format!("{rows:?}").contains("hunter2"));
    }

    #[test]
    fn the_writer_and_driver_never_log_themselves() {
        let rows = capture(|| {
            tracing::warn!(target: "sqlx::query", "slow statement");
            tracing::warn!(target: "sqlx", "pool timed out");
        });
        assert!(rows.is_empty());
    }

    #[test]
    fn long_values_are_truncated() {
        let long = "x".repeat(MAX_VALUE + 50);
        let rows = capture(|| tracing::warn!(detail = %long, "long"));
        assert_eq!(
            rows[0].context["detail"].chars().count(),
            MAX_VALUE + 1,
            "the value and an ellipsis"
        );
    }
}
