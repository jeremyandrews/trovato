//! Precise timers beside cron (B278).
//!
//! Cron runs every task on a cadence; a timer runs one piece of work at a
//! known second. Kernel code and plugins register a timer for an exact time,
//! optionally repeating every so many seconds, and it fires once at that time
//! across any number of kernel instances. Cron stays for routine sweeps.
//!
//! # Storage
//!
//! One `scheduled_timer` row per timer, unique per `(owner, name)`. The owner
//! is [`KERNEL_OWNER`] or a plugin's machine name. Registering the same name
//! again replaces the row, so registration is idempotent.
//!
//! # The scheduler
//!
//! [`TimerService::run_scheduler`] is one task per kernel process. It sleeps
//! until the earliest scheduled timer is due, claims what is due with one
//! `FOR UPDATE SKIP LOCKED` statement, so two servers never claim the same row,
//! and fires each claim on its own task, at most `max_concurrent` at once. A
//! write that moves a timer earlier than the current wake time wakes it at
//! once: in this process through a [`Notify`], and on every server through
//! Postgres `NOTIFY` on [`NOTIFY_CHANNEL`], sent in the write's own
//! transaction. It rechecks at least every [`SAFETY_RECHECK`] in case a
//! notification was lost, so an idle site with no timers runs one indexed
//! query a minute and nothing else.
//!
//! # Firing
//!
//! A plugin's timer dispatches `tap_timer` to that plugin only, as a
//! background tap under the background CPU and wall clock limits. A kernel
//! timer calls the Rust handler registered for it with
//! [`TimerService::register_handler`]. A failure is retried with the kernel's
//! exponential backoff (`crate::cron::exponential_backoff_secs`) up to
//! [`MAX_ATTEMPTS`], then the timer is `failed`, with the error kept.
//!
//! A one-shot timer is deleted after it succeeds. A repeating one moves to its
//! next slot after the current time, counted in whole intervals from its
//! schedule rather than from when it fired, so it keeps its cadence, and after
//! downtime the missed slots fire once, not once each.
//!
//! A claim carries a lease. A server that stops while a timer runs leaves the
//! row `running`; once the lease is over any server claims it again. So a
//! timer fires exactly once unless its server dies between finishing the work
//! and recording that it did, when it fires again: handlers must be
//! idempotent, as queue workers are.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::cron::CronService;
use crate::tap::DispatchOutcome;

/// The owner of the kernel's own timers.
pub const KERNEL_OWNER: &str = "kernel";

/// The Postgres channel every timer write notifies, with the timer's `run_at`
/// in Unix milliseconds as the payload.
pub const NOTIFY_CHANNEL: &str = "trovato_timer";

/// `site_config` key holding the throttles, a JSON object of
/// [`TimerSettings`]; edited on `/admin/reports/timers/settings`.
pub const TIMER_SETTINGS_KEY: &str = "timer_settings";

/// Attempts before a failing timer is left `failed`.
pub const MAX_ATTEMPTS: i32 = 3;

/// First retry delay, seconds; doubled per attempt up to [`RETRY_CAP_SECS`].
pub const RETRY_BASE_SECS: i64 = 30;

/// Longest retry delay, seconds.
pub const RETRY_CAP_SECS: i64 = 3600;

/// Longest the scheduler sleeps without looking, whatever it was told.
pub const SAFETY_RECHECK: Duration = Duration::from_secs(60);

/// Longest timer name, in bytes.
pub const MAX_NAME_BYTES: usize = 255;

/// The claim lease when no plugin runtime says how long a background tap may
/// run: the shipped background wall clock ceiling (900 seconds) and a minute.
const DEFAULT_LEASE_SECS: u64 = 960;

/// The longest error kept on a timer, in bytes: enough for any message, and
/// small enough that a timer read back fits a plugin's output buffer.
pub const MAX_ERROR_BYTES: usize = 2048;

/// `text` cut to at most `max` bytes, on a character boundary.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// How long the scheduler keeps the concurrency throttle before reading it
/// again while timers are due.
const SETTINGS_REREAD: Duration = Duration::from_secs(1);

/// Largest repeat interval: what fits the column.
const MAX_REPEAT_SECS: i64 = i32::MAX as i64;

/// The scheduler's wake query (`$1` an owner scope or NULL): milliseconds
/// until the earliest scheduled timer or the earliest lease to run out. Public
/// so the benchmark explains exactly this statement.
pub const NEXT_DUE_SQL: &str = "SELECT (EXTRACT(EPOCH FROM (LEAST( \
     (SELECT min(run_at) FROM scheduled_timer \
      WHERE status = 'scheduled' AND ($1::text[] IS NULL OR owner = ANY($1))), \
     (SELECT min(claimed_until) FROM scheduled_timer \
      WHERE status = 'running' AND ($1::text[] IS NULL OR owner = ANY($1))) \
     ) - clock_timestamp())) * 1000)::float8";

/// The claim (`$1` scope, `$2` limit, `$3` this server, `$4` lease seconds):
/// due timers, and any whose lease ran out, in one statement. Public so the
/// benchmark explains exactly this statement.
pub const CLAIM_SQL: &str = "WITH due AS ( \
     SELECT id FROM scheduled_timer \
     WHERE status = 'scheduled' AND run_at <= clock_timestamp() \
       AND ($1::text[] IS NULL OR owner = ANY($1)) \
     ORDER BY run_at LIMIT $2 FOR UPDATE SKIP LOCKED \
     ), stale AS ( \
     SELECT id FROM scheduled_timer \
     WHERE status = 'running' AND claimed_until < clock_timestamp() \
       AND ($1::text[] IS NULL OR owner = ANY($1)) \
     ORDER BY claimed_until LIMIT $2 FOR UPDATE SKIP LOCKED \
     ), picked AS ( \
     SELECT id FROM due UNION ALL SELECT id FROM stale LIMIT $2 \
     ) \
     UPDATE scheduled_timer t SET \
     status = 'running', attempts = t.attempts + 1, \
     claim_token = gen_random_uuid(), claimed_by = $3, \
     claimed_until = clock_timestamp() + make_interval(secs => $4), \
     last_fired_at = clock_timestamp(), \
     last_lateness_ms = GREATEST(0, (EXTRACT(EPOCH FROM (clock_timestamp() - t.run_at)) * 1000)::bigint), \
     changed = now() \
     FROM picked d WHERE t.id = d.id \
     RETURNING t.id, t.owner, t.name, t.payload, t.run_at, t.last_fired_at AS fired_at, \
     t.repeat_secs, t.attempts, t.claim_token, t.last_lateness_ms AS lateness_ms";

// =============================================================================
// Settings
// =============================================================================

/// The throttles. Each is a site setting with an environment override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimerSettings {
    /// Most timers one server fires at once.
    pub max_concurrent: u32,
    /// Most timers one plugin may hold, whatever their status.
    pub max_per_plugin: u32,
    /// Shortest repeat interval a timer may ask for, seconds.
    pub min_repeat_secs: u32,
    /// Largest payload, bytes of JSON.
    pub max_payload_bytes: u32,
}

/// The largest payload limit a site may set, in bytes. A plugin reads a timer
/// back through a fixed output buffer (256 KiB in the SDK); a payload up to
/// this, with the name and the error kept on the timer, always fits it.
pub const MAX_PAYLOAD_LIMIT: u32 = 65_536;

impl Default for TimerSettings {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            max_per_plugin: 10_000,
            min_repeat_secs: 60,
            max_payload_bytes: 4096,
        }
    }
}

/// Environment overrides of [`TimerSettings`], read once at boot by
/// `Config::from_env`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimerOverrides {
    /// `TIMER_MAX_CONCURRENT`.
    pub max_concurrent: Option<u32>,
    /// `TIMER_MAX_PER_PLUGIN`.
    pub max_per_plugin: Option<u32>,
    /// `TIMER_MIN_REPEAT_SECS`.
    pub min_repeat_secs: Option<u32>,
    /// `TIMER_MAX_PAYLOAD_BYTES`.
    pub max_payload_bytes: Option<u32>,
}

impl TimerOverrides {
    /// The environment variables, in [`TimerSettings`] field order.
    pub const VARIABLES: [&'static str; 4] = [
        "TIMER_MAX_CONCURRENT",
        "TIMER_MAX_PER_PLUGIN",
        "TIMER_MIN_REPEAT_SECS",
        "TIMER_MAX_PAYLOAD_BYTES",
    ];

    /// Read the overrides. An empty variable is unset; one that is not a
    /// whole number above zero stops the server starting, as a bad
    /// `CRON_INTERVAL_SECS` does.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let read = |name: &str| -> Result<Option<u32>> {
            match lookup(name).map(|v| v.trim().to_string()) {
                None => Ok(None),
                Some(v) if v.is_empty() => Ok(None),
                Some(v) => match v.parse::<u32>() {
                    Ok(n) if n > 0 => Ok(Some(n)),
                    _ => anyhow::bail!("{name} must be a whole number above 0"),
                },
            }
        };
        let max_payload_bytes = read(Self::VARIABLES[3])?;
        if max_payload_bytes.is_some_and(|n| n > MAX_PAYLOAD_LIMIT) {
            anyhow::bail!("{} must be at most {MAX_PAYLOAD_LIMIT}", Self::VARIABLES[3]);
        }
        Ok(Self {
            max_concurrent: read(Self::VARIABLES[0])?,
            max_per_plugin: read(Self::VARIABLES[1])?,
            min_repeat_secs: read(Self::VARIABLES[2])?,
            max_payload_bytes,
        })
    }

    /// `settings` with every override applied.
    pub fn apply(&self, settings: TimerSettings) -> TimerSettings {
        TimerSettings {
            max_concurrent: self.max_concurrent.unwrap_or(settings.max_concurrent),
            max_per_plugin: self.max_per_plugin.unwrap_or(settings.max_per_plugin),
            min_repeat_secs: self.min_repeat_secs.unwrap_or(settings.min_repeat_secs),
            max_payload_bytes: self.max_payload_bytes.unwrap_or(settings.max_payload_bytes),
        }
    }

    /// Whether each field is overridden, in [`Self::VARIABLES`] order.
    pub fn set(&self) -> [bool; 4] {
        [
            self.max_concurrent.is_some(),
            self.max_per_plugin.is_some(),
            self.min_repeat_secs.is_some(),
            self.max_payload_bytes.is_some(),
        ]
    }
}

/// This process's environment overrides, set once at boot.
static ENV_OVERRIDES: OnceLock<TimerOverrides> = OnceLock::new();

/// Record the environment overrides read at boot. The first call wins.
pub fn set_env_overrides(overrides: TimerOverrides) {
    let _ = ENV_OVERRIDES.set(overrides);
}

/// The environment overrides in force (none before boot sets them).
pub fn env_overrides() -> TimerOverrides {
    ENV_OVERRIDES.get().copied().unwrap_or_default()
}

/// The throttles as saved, before the environment: the `timer_settings` row
/// over the defaults. A field that is missing or zero keeps its default.
pub async fn saved_settings(pool: &PgPool) -> Result<TimerSettings> {
    let defaults = TimerSettings::default();
    let Some(value) = crate::models::SiteConfig::get(pool, TIMER_SETTINGS_KEY).await? else {
        return Ok(defaults);
    };
    let saved: TimerSettings = serde_json::from_value(value).unwrap_or(defaults);
    let or = |v: u32, d: u32| if v == 0 { d } else { v };
    Ok(TimerSettings {
        max_concurrent: or(saved.max_concurrent, defaults.max_concurrent),
        max_per_plugin: or(saved.max_per_plugin, defaults.max_per_plugin),
        min_repeat_secs: or(saved.min_repeat_secs, defaults.min_repeat_secs),
        max_payload_bytes: or(saved.max_payload_bytes, defaults.max_payload_bytes)
            .min(MAX_PAYLOAD_LIMIT),
    })
}

/// The throttles in force: saved settings with the environment applied.
pub async fn effective_settings(pool: &PgPool) -> Result<TimerSettings> {
    Ok(env_overrides().apply(saved_settings(pool).await?))
}

/// Save the throttles.
pub async fn save_settings(pool: &PgPool, settings: TimerSettings) -> Result<()> {
    crate::models::SiteConfig::set(pool, TIMER_SETTINGS_KEY, serde_json::to_value(settings)?).await
}

/// [`save_settings`], writing `event` (the `timer.settings_changed`
/// recording the change) in the same transaction: the settings and their
/// event commit together, or neither does.
pub async fn save_settings_recorded(
    pool: &PgPool,
    settings: TimerSettings,
    audit: &crate::audit::SecurityAudit,
    event: crate::audit::SecurityEvent,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    crate::models::SiteConfig::set_in(
        &mut *tx,
        TIMER_SETTINGS_KEY,
        serde_json::to_value(settings)?,
    )
    .await?;
    audit.record_in(&mut tx, event).await?;
    tx.commit().await?;
    // Every server drops its in-memory site config, as `SiteConfig::set` does.
    crate::cache::generation::bump(crate::cache::generation::CacheKind::SiteConfig);
    Ok(())
}

// =============================================================================
// Timers
// =============================================================================

/// A timer's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatus {
    /// Waiting for `run_at`.
    Scheduled,
    /// Held by an administrator; fires only once resumed.
    Paused,
    /// Claimed by a server and firing.
    Running,
    /// Failed [`MAX_ATTEMPTS`] times; fires only once resumed.
    Failed,
}

impl TimerStatus {
    /// Every status, in the order the admin filter lists them.
    pub const ALL: [TimerStatus; 4] = [
        TimerStatus::Scheduled,
        TimerStatus::Paused,
        TimerStatus::Running,
        TimerStatus::Failed,
    ];

    /// The stored word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Paused => "paused",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }

    /// The status a stored word names.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == s)
    }
}

/// A stored timer.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Timer {
    pub id: i64,
    pub owner: String,
    pub name: String,
    /// When the next attempt is due.
    pub run_at: DateTime<Utc>,
    /// The schedule slot the next attempt belongs to; differs from `run_at`
    /// only while a failure is being retried.
    pub slot_at: DateTime<Utc>,
    pub repeat_secs: Option<i32>,
    pub payload: serde_json::Value,
    pub status: String,
    pub attempts: i32,
    pub claimed_by: Option<String>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub last_lateness_ms: Option<i64>,
    pub last_error: Option<String>,
    pub created: DateTime<Utc>,
    pub changed: DateTime<Utc>,
}

/// A statement built from this file's own constants: the column list, the
/// fixed filter, and a sort from the admin listing's fixed table. No value
/// from a caller is ever formatted in; values are bound.
macro_rules! safe_sql {
    ($($t:tt)*) => {
        sqlx::AssertSqlSafe(format!($($t)*))
    };
}

/// The columns [`Timer`] reads.
const TIMER_COLUMNS: &str = "id, owner, name, run_at, slot_at, repeat_secs, payload, status, \
     attempts, claimed_by, last_fired_at, last_lateness_ms, last_error, created, changed";

/// A registration: what kernel code and the `timer.set` host function pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimerRequest {
    /// Unique per owner; registering it again replaces the timer.
    pub name: String,
    /// When it fires, Unix seconds (UTC). A time already past fires at once.
    pub run_at: i64,
    /// Fire again every this many seconds; absent for a one-shot timer.
    #[serde(default)]
    pub repeat_secs: Option<i64>,
    /// Handed back to the handler when it fires.
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Why a registration or an administrator's change was refused.
#[derive(Debug, thiserror::Error)]
pub enum TimerRefused {
    /// Empty, too long, or holding a control character.
    #[error("a timer name must be 1 to {MAX_NAME_BYTES} bytes with no control characters")]
    InvalidName,
    /// Not a time that can be stored.
    #[error("the run time is not a valid time")]
    InvalidTime,
    /// Over `max_payload_bytes`.
    #[error("the payload is {size} bytes; the most allowed is {max}")]
    PayloadTooLarge { size: usize, max: u32 },
    /// Under `min_repeat_secs`, or not a positive number.
    #[error("the repeat interval must be at least {min} seconds")]
    RepeatTooShort { min: u32 },
    /// The owner holds `max_per_plugin` timers already.
    #[error("{owner} already has {max} timers, the most allowed")]
    TooMany { owner: String, max: u32 },
    /// No such timer.
    #[error("no such timer")]
    NotFound,
    /// The database failed.
    #[error("the timer could not be stored: {0}")]
    Storage(#[from] anyhow::Error),
}

impl From<sqlx::Error> for TimerRefused {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.into())
    }
}

/// Check a name.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control)
}

/// Check a repeat interval against the minimum.
fn check_repeat(
    repeat_secs: Option<i64>,
    settings: &TimerSettings,
) -> Result<Option<i32>, TimerRefused> {
    match repeat_secs {
        None => Ok(None),
        Some(secs)
            if secs >= i64::from(settings.min_repeat_secs.max(1)) && secs <= MAX_REPEAT_SECS =>
        {
            Ok(Some(secs as i32))
        }
        Some(_) => Err(TimerRefused::RepeatTooShort {
            min: settings.min_repeat_secs.max(1),
        }),
    }
}

/// A Unix second as a time.
fn unix_time(secs: i64) -> Result<DateTime<Utc>, TimerRefused> {
    DateTime::from_timestamp(secs, 0).ok_or(TimerRefused::InvalidTime)
}

/// Register or replace a timer for `owner`, checked against `settings`.
///
/// The per-owner count is not applied to [`KERNEL_OWNER`]. A paused timer
/// registered again keeps its pause with the new time: the administrator's
/// hold outlasts the code's update. Notifies every server in the same
/// transaction.
pub async fn set(
    pool: &PgPool,
    owner: &str,
    request: &TimerRequest,
    settings: &TimerSettings,
) -> Result<Timer, TimerRefused> {
    if !valid_name(&request.name) || !valid_name(owner) {
        return Err(TimerRefused::InvalidName);
    }
    let run_at = unix_time(request.run_at)?;
    let repeat_secs = check_repeat(request.repeat_secs, settings)?;
    let payload = if request.payload.is_null() {
        serde_json::json!({})
    } else {
        request.payload.clone()
    };
    let size = serde_json::to_string(&payload)
        .map(|s| s.len())
        .unwrap_or(usize::MAX);
    let max = settings.max_payload_bytes.min(MAX_PAYLOAD_LIMIT);
    if size > max as usize {
        return Err(TimerRefused::PayloadTooLarge { size, max });
    }

    let mut tx = pool.begin().await?;
    if owner != KERNEL_OWNER {
        // One registration per owner at a time, so two concurrent ones cannot
        // both pass the count.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('scheduled_timer:' || $1))")
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        // Replacing a timer the owner holds adds none, so only a new name is
        // held to the limit: an owner over a lowered limit can still move the
        // timers it has.
        let (held, exists): (i64, bool) = sqlx::query_as(
            "SELECT count(*), coalesce(bool_or(name = $2), false) \
             FROM scheduled_timer WHERE owner = $1",
        )
        .bind(owner)
        .bind(&request.name)
        .fetch_one(&mut *tx)
        .await?;
        if !exists && held >= i64::from(settings.max_per_plugin) {
            return Err(TimerRefused::TooMany {
                owner: owner.to_string(),
                max: settings.max_per_plugin,
            });
        }
    }
    let timer: Timer = sqlx::query_as(safe_sql!(
        "INSERT INTO scheduled_timer (owner, name, run_at, slot_at, repeat_secs, payload) \
         VALUES ($1, $2, $3, $3, $4, $5) \
         ON CONFLICT (owner, name) DO UPDATE SET \
             run_at = EXCLUDED.run_at, slot_at = EXCLUDED.slot_at, \
             repeat_secs = EXCLUDED.repeat_secs, payload = EXCLUDED.payload, \
             status = CASE WHEN scheduled_timer.status = 'paused' THEN 'paused' \
                           ELSE 'scheduled' END, \
             attempts = 0, last_error = NULL, claim_token = NULL, claimed_by = NULL, \
             claimed_until = NULL, changed = now() \
         RETURNING {TIMER_COLUMNS}"
    ))
    .bind(owner)
    .bind(&request.name)
    .bind(run_at)
    .bind(repeat_secs)
    .bind(&payload)
    .fetch_one(&mut *tx)
    .await?;
    notify(&mut tx, timer.run_at).await?;
    tx.commit().await?;
    wake_local(timer.run_at.timestamp_millis());
    Ok(timer)
}

// =============================================================================
// Waking the loops in this process
// =============================================================================

/// A running scheduler loop in this process: its wake-up and the time it
/// will next look.
struct LocalLoop {
    notify: Weak<Notify>,
    wake_at_ms: Weak<AtomicI64>,
}

/// Every scheduler loop running in this process.
static LOCAL_LOOPS: Mutex<Vec<LocalLoop>> = Mutex::new(Vec::new());

/// Wake each loop in this process that would next look after `at_ms`.
///
/// Every write that can move a timer earlier calls this after its commit, so
/// a timer set on this server (a plugin's, through the host interface, or an
/// administrator's) wakes this server's loop at once even when its `LISTEN`
/// connection is down. The `NOTIFY` in the same write reaches the other
/// servers.
fn wake_local(at_ms: i64) {
    let Ok(mut loops) = LOCAL_LOOPS.lock() else {
        return;
    };
    loops.retain(|l| {
        let (Some(notify), Some(wake_at)) = (l.notify.upgrade(), l.wake_at_ms.upgrade()) else {
            return false;
        };
        if at_ms < wake_at.load(Ordering::SeqCst) {
            notify.notify_one();
        }
        true
    });
}

/// Send the wake-up for a write due at `at`, in the write's transaction.
async fn notify(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, at: DateTime<Utc>) -> Result<()> {
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(NOTIFY_CHANNEL)
        .bind(at.timestamp_millis().to_string())
        .execute(&mut **tx)
        .await
        .context("notify the timer schedulers")?;
    Ok(())
}

/// Cancel `owner`'s timer `name`. Returns whether there was one.
///
/// One statement, the delete and its notification together: an item save
/// with no date to time cancels blindly, and that should cost one round trip.
pub async fn cancel(pool: &PgPool, owner: &str, name: &str) -> Result<bool> {
    let gone: Option<i64> = sqlx::query_scalar(
        "WITH gone AS (DELETE FROM scheduled_timer WHERE owner = $1 AND name = $2 \
         RETURNING run_at) \
         SELECT count(*) FROM gone, \
         LATERAL pg_notify($3, (EXTRACT(EPOCH FROM gone.run_at) * 1000)::bigint::text)",
    )
    .bind(owner)
    .bind(name)
    .bind(NOTIFY_CHANNEL)
    .fetch_optional(pool)
    .await?;
    Ok(gone.unwrap_or(0) > 0)
}

/// `owner`'s timer `name`.
pub async fn get(pool: &PgPool, owner: &str, name: &str) -> Result<Option<Timer>> {
    Ok(sqlx::query_as(safe_sql!(
        "SELECT {TIMER_COLUMNS} FROM scheduled_timer WHERE owner = $1 AND name = $2"
    ))
    .bind(owner)
    .bind(name)
    .fetch_optional(pool)
    .await?)
}

/// The timer with this id.
pub async fn find(pool: &PgPool, id: i64) -> Result<Option<Timer>> {
    Ok(sqlx::query_as(safe_sql!(
        "SELECT {TIMER_COLUMNS} FROM scheduled_timer WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// What the admin listing filters on.
#[derive(Debug, Clone, Default)]
pub struct TimerFilter {
    /// Exactly this owner.
    pub owner: Option<String>,
    /// Exactly this status.
    pub status: Option<TimerStatus>,
    /// Name contains, as an escaped `ILIKE` pattern.
    pub name_like: Option<String>,
}

/// One page of timers matching `filter`, soonest first, and how many match.
pub async fn list(
    pool: &PgPool,
    filter: &TimerFilter,
    order_by: &str,
    limit: i64,
    offset: i64,
) -> Result<(Vec<Timer>, i64)> {
    // `order_by` comes only from the listing's fixed sort table.
    let where_sql = "($1::text IS NULL OR owner = $1) \
         AND ($2::text IS NULL OR status = $2) \
         AND ($3::text IS NULL OR name ILIKE $3)";
    let status = filter.status.map(TimerStatus::as_str);
    let rows: Vec<Timer> = sqlx::query_as(safe_sql!(
        "SELECT {TIMER_COLUMNS} FROM scheduled_timer WHERE {where_sql} \
         ORDER BY {order_by}, id LIMIT $4 OFFSET $5"
    ))
    .bind(&filter.owner)
    .bind(status)
    .bind(&filter.name_like)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let total: i64 = sqlx::query_scalar(safe_sql!(
        "SELECT count(*) FROM scheduled_timer WHERE {where_sql}"
    ))
    .bind(&filter.owner)
    .bind(status)
    .bind(&filter.name_like)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// Every owner that holds a timer, for the listing's owner filter.
pub async fn owners(pool: &PgPool) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT DISTINCT owner FROM scheduled_timer ORDER BY owner")
            .fetch_all(pool)
            .await?,
    )
}

/// An administrator's change to one timer.
#[derive(Debug, Clone, PartialEq)]
pub enum AdminChange {
    /// Hold it: it does not fire until resumed.
    Pause,
    /// Schedule it again, from paused or failed, with its attempts reset.
    Resume,
    /// Fire it as soon as a scheduler looks, keeping a repeat's cadence.
    RunNow,
    /// Delete it.
    Cancel,
    /// Move the next run, and set or clear the repeat interval.
    Edit {
        run_at: i64,
        repeat_secs: Option<i64>,
    },
}

impl AdminChange {
    /// The word the security log records.
    pub fn action(&self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::RunNow => "run_now",
            Self::Cancel => "cancel",
            Self::Edit { .. } => "edit",
        }
    }
}

/// Apply an administrator's change to timer `id`. Returns the timer before
/// and, unless it was cancelled, after. Pausing a running timer releases its
/// claim, so the server firing it records nothing over the pause.
pub async fn admin_change(
    pool: &PgPool,
    id: i64,
    change: &AdminChange,
    settings: &TimerSettings,
) -> Result<(Timer, Option<Timer>), TimerRefused> {
    admin_change_recorded(pool, id, change, settings, None).await
}

/// What records an administrator's change to a timer: the stream, and the
/// event it writes given the timer before and, unless cancelled, after.
pub type ChangeRecord<'a> = (
    &'a crate::audit::SecurityAudit,
    Box<dyn FnOnce(&Timer, Option<&Timer>) -> crate::audit::SecurityEvent + Send + 'a>,
);

/// [`admin_change`], writing the event `record` makes (the `timer.changed`
/// recording the change) in the change's own transaction: the change and its
/// event commit together, or neither does.
pub async fn admin_change_recorded(
    pool: &PgPool,
    id: i64,
    change: &AdminChange,
    settings: &TimerSettings,
    record: Option<ChangeRecord<'_>>,
) -> Result<(Timer, Option<Timer>), TimerRefused> {
    let mut tx = pool.begin().await?;
    let before: Timer = sqlx::query_as(safe_sql!(
        "SELECT {TIMER_COLUMNS} FROM scheduled_timer WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(TimerRefused::NotFound)?;
    let released = "claim_token = NULL, claimed_by = NULL, claimed_until = NULL, changed = now()";
    let after: Option<Timer> = match change {
        AdminChange::Cancel => {
            sqlx::query("DELETE FROM scheduled_timer WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            None
        }
        AdminChange::Pause => Some(
            sqlx::query_as(safe_sql!(
                "UPDATE scheduled_timer SET status = 'paused', {released} \
                 WHERE id = $1 RETURNING {TIMER_COLUMNS}"
            ))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?,
        ),
        AdminChange::Resume => Some(
            sqlx::query_as(safe_sql!(
                "UPDATE scheduled_timer SET status = 'scheduled', attempts = 0, {released} \
                 WHERE id = $1 RETURNING {TIMER_COLUMNS}"
            ))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?,
        ),
        AdminChange::RunNow => Some(
            sqlx::query_as(safe_sql!(
                "UPDATE scheduled_timer SET status = 'scheduled', attempts = 0, \
                 run_at = date_trunc('second', clock_timestamp()), {released} \
                 WHERE id = $1 RETURNING {TIMER_COLUMNS}"
            ))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?,
        ),
        AdminChange::Edit {
            run_at,
            repeat_secs,
        } => {
            let run_at = unix_time(*run_at)?;
            let repeat_secs = check_repeat(*repeat_secs, settings)?;
            Some(
                sqlx::query_as(safe_sql!(
                    "UPDATE scheduled_timer SET run_at = $2, slot_at = $2, repeat_secs = $3, \
                     status = CASE WHEN status = 'paused' THEN 'paused' ELSE 'scheduled' END, \
                     attempts = 0, {released} \
                     WHERE id = $1 RETURNING {TIMER_COLUMNS}"
                ))
                .bind(id)
                .bind(run_at)
                .bind(repeat_secs)
                .fetch_one(&mut *tx)
                .await?,
            )
        }
    };
    notify(&mut tx, after.as_ref().map_or(before.run_at, |t| t.run_at)).await?;
    if let Some((audit, event)) = record {
        audit
            .record_in(&mut tx, event(&before, after.as_ref()))
            .await?;
    }
    tx.commit().await?;
    if let Some(t) = &after {
        wake_local(t.run_at.timestamp_millis());
    }
    Ok((before, after))
}

// =============================================================================
// The service and its scheduler
// =============================================================================

/// What a handler is told about the timer it fires for.
#[derive(Debug, Clone, Serialize)]
pub struct FiredTimer {
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub payload: serde_json::Value,
    /// When this attempt was due.
    pub run_at: DateTime<Utc>,
    /// When it was claimed.
    pub fired_at: DateTime<Utc>,
    pub repeat_secs: Option<i32>,
    /// This attempt, from 1.
    pub attempt: i32,
}

/// The future a kernel handler returns: `Err` is a failed attempt, with the
/// error kept on the timer.
pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// A Rust handler for kernel timers.
pub type KernelHandler = Arc<dyn Fn(FiredTimer) -> HandlerFuture + Send + Sync>;

/// A claimed row.
#[derive(Debug, sqlx::FromRow)]
struct Claimed {
    id: i64,
    owner: String,
    name: String,
    payload: serde_json::Value,
    run_at: DateTime<Utc>,
    fired_at: DateTime<Utc>,
    repeat_secs: Option<i32>,
    attempts: i32,
    claim_token: Uuid,
    lateness_ms: i64,
}

/// The kernel's timer service: registration with the in-process wake-up, the
/// handlers, and the scheduler loop.
pub struct TimerService {
    pool: PgPool,
    /// Background plugin dispatch, shared with cron (`tap_timer` reaches a
    /// plugin as `tap_cron` and `tap_queue_worker` do).
    cron: Option<Arc<CronService>>,
    /// Rust handlers by owner and name prefix.
    handlers: RwLock<HashMap<String, Vec<(String, KernelHandler)>>>,
    /// Wakes the loop in this process.
    notify: Arc<Notify>,
    /// The wall clock time, Unix ms, the loop will next look at the table;
    /// `i64::MAX` while it is looking, so any write in between wakes it.
    wake_at_ms: Arc<AtomicI64>,
    /// Names this server's claims.
    instance: String,
    /// Restrict claims to these owners. `None`, the default, claims every
    /// owner's timers; tests sharing one database scope each scheduler to the
    /// owners they made.
    owner_scope: RwLock<Option<Vec<String>>>,
    /// Throttles to use instead of the stored ones (tests).
    settings_override: RwLock<Option<TimerSettings>>,
    /// Retry backoff base and cap, seconds.
    retry: RwLock<(i64, i64)>,
    /// Timers this service has fired, whatever became of them.
    fired: AtomicU64,
    /// Whether the loop listens for other servers' writes (tests turn it off
    /// to show what the in-process wake-up does alone).
    listen: AtomicBool,
    /// Called just before each claim (tests use it to land a shutdown there).
    before_claim: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for TimerService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimerService")
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

impl TimerService {
    /// A service over `pool`. `cron` dispatches plugin timers; without it a
    /// plugin's timer fails with "no plugin dispatcher".
    pub fn new(pool: PgPool, cron: Option<Arc<CronService>>) -> Self {
        let host = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        Self {
            pool,
            cron,
            handlers: RwLock::new(HashMap::new()),
            notify: Arc::new(Notify::new()),
            wake_at_ms: Arc::new(AtomicI64::new(i64::MAX)),
            instance: format!("{host}:{}", &Uuid::new_v4().simple().to_string()[..8]),
            owner_scope: RwLock::new(None),
            settings_override: RwLock::new(None),
            retry: RwLock::new((RETRY_BASE_SECS, RETRY_CAP_SECS)),
            fired: AtomicU64::new(0),
            listen: AtomicBool::new(true),
            before_claim: RwLock::new(None),
        }
    }

    /// This server's name on its claims.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// How many timers this service has fired.
    pub fn fired_count(&self) -> u64 {
        self.fired.load(Ordering::Relaxed)
    }

    /// Call `handler` for `owner`'s timers named `prefix` or starting with
    /// `prefix:`. Kernel code registers under [`KERNEL_OWNER`].
    pub fn register_handler(&self, owner: &str, prefix: &str, handler: KernelHandler) {
        if let Ok(mut handlers) = self.handlers.write() {
            handlers
                .entry(owner.to_string())
                .or_default()
                .push((prefix.to_string(), handler));
        }
    }

    /// The handler for `owner`'s timer `name`, if any.
    fn handler_for(&self, owner: &str, name: &str) -> Option<KernelHandler> {
        let handlers = self.handlers.read().ok()?;
        handlers.get(owner)?.iter().find_map(|(prefix, handler)| {
            let matches = name == prefix
                || name
                    .strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with(':'));
            matches.then(|| handler.clone())
        })
    }

    /// Restrict claims to these owners (tests that share a database).
    pub fn set_owner_scope(&self, owners: Option<Vec<String>>) {
        if let Ok(mut scope) = self.owner_scope.write() {
            *scope = owners;
        }
    }

    /// Use these throttles instead of the stored ones (tests).
    pub fn set_settings_override(&self, settings: Option<TimerSettings>) {
        if let Ok(mut current) = self.settings_override.write() {
            *current = settings;
        }
    }

    /// Retry backoff base and cap, seconds (tests shorten it).
    pub fn set_retry_backoff(&self, base_secs: i64, cap_secs: i64) {
        if let Ok(mut retry) = self.retry.write() {
            *retry = (base_secs.max(0), cap_secs.max(0));
        }
    }

    /// Turn off listening for other servers' writes (tests). Takes effect
    /// for a loop started afterwards.
    pub fn set_listen(&self, on: bool) {
        self.listen.store(on, Ordering::SeqCst);
    }

    /// Run `hook` just before each claim (tests).
    pub fn set_before_claim_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Ok(mut current) = self.before_claim.write() {
            *current = hook;
        }
    }

    /// The throttles this service applies.
    pub async fn settings(&self) -> Result<TimerSettings> {
        if let Some(settings) = self.settings_override.read().ok().and_then(|s| *s) {
            return Ok(settings);
        }
        effective_settings(&self.pool).await
    }

    /// Register or replace `owner`'s timer, with this service's throttles. Like
    /// every write, it wakes the loops in this process that would look later.
    pub async fn set(&self, owner: &str, request: &TimerRequest) -> Result<Timer, TimerRefused> {
        let settings = self.settings().await?;
        set(&self.pool, owner, request, &settings).await
    }

    /// Cancel `owner`'s timer `name`.
    pub async fn cancel(&self, owner: &str, name: &str) -> Result<bool> {
        cancel(&self.pool, owner, name).await
    }

    /// `owner`'s timer `name`.
    pub async fn get(&self, owner: &str, name: &str) -> Result<Option<Timer>> {
        get(&self.pool, owner, name).await
    }

    /// An administrator's change, with this service's throttles.
    pub async fn admin_change(
        &self,
        id: i64,
        change: &AdminChange,
    ) -> Result<(Timer, Option<Timer>), TimerRefused> {
        let settings = self.settings().await?;
        admin_change(&self.pool, id, change, &settings).await
    }

    /// [`Self::admin_change`], with its event written in the change's own
    /// transaction (see [`admin_change_recorded`]).
    pub async fn admin_change_recorded(
        &self,
        id: i64,
        change: &AdminChange,
        record: ChangeRecord<'_>,
    ) -> Result<(Timer, Option<Timer>), TimerRefused> {
        let settings = self.settings().await?;
        admin_change_recorded(&self.pool, id, change, &settings, Some(record)).await
    }

    /// Wake the loop if `at_ms` is before the time it will next look.
    fn wake_if_before(&self, at_ms: i64) {
        if at_ms < self.wake_at_ms.load(Ordering::SeqCst) {
            self.notify.notify_one();
        }
    }

    /// The claim lease: how long a background tap may run, and a minute.
    fn lease_secs(&self) -> u64 {
        self.cron
            .as_ref()
            .and_then(|cron| cron.background_wallclock_ceiling_secs())
            .map_or(DEFAULT_LEASE_SECS, |ceiling| ceiling.saturating_add(60))
    }

    /// The owners this scheduler claims for, as a bind value.
    fn scope(&self) -> Option<Vec<String>> {
        self.owner_scope.read().ok().and_then(|s| s.clone())
    }

    /// Milliseconds until the scheduler next has something to do: the
    /// earliest scheduled timer, or the earliest lease to run out. `None`
    /// when there is neither. One statement on the two partial indexes.
    pub async fn next_due_in_ms(&self) -> Result<Option<i64>> {
        let ms: Option<f64> = sqlx::query_scalar(NEXT_DUE_SQL)
            .bind(self.scope())
            .fetch_one(&self.pool)
            .await
            .context("read the next timer")?;
        Ok(ms.map(|ms| ms.ceil() as i64))
    }

    /// Claim up to `limit` due timers, and any whose lease ran out, in one
    /// statement. `FOR UPDATE SKIP LOCKED`: a row another server is claiming
    /// is passed over, never claimed twice.
    async fn claim(&self, limit: i64) -> Result<Vec<Claimed>> {
        let rows: Vec<Claimed> = sqlx::query_as(CLAIM_SQL)
            .bind(self.scope())
            .bind(limit)
            .bind(&self.instance)
            .bind(self.lease_secs() as f64)
            .fetch_all(&self.pool)
            .await
            .context("claim due timers")?;
        Ok(rows)
    }

    /// Run one claimed timer and record what became of it.
    async fn fire(self: Arc<Self>, claimed: Claimed) {
        self.fired.fetch_add(1, Ordering::Relaxed);
        let fired = FiredTimer {
            id: claimed.id,
            owner: claimed.owner.clone(),
            name: claimed.name.clone(),
            payload: claimed.payload.clone(),
            run_at: claimed.run_at,
            fired_at: claimed.fired_at,
            repeat_secs: claimed.repeat_secs,
            attempt: claimed.attempts,
        };
        let started = std::time::Instant::now();
        let outcome = if claimed.attempts > MAX_ATTEMPTS {
            // Claimed again after a lease ran out more often than a failure
            // would have been retried: its server keeps dying on it.
            Err(format!(
                "claimed {} times and never finished; the server running it stopped",
                claimed.attempts
            ))
        } else if let Some(handler) = self.handler_for(&claimed.owner, &claimed.name) {
            let budget = Duration::from_secs(self.lease_secs().saturating_sub(30).max(1));
            match tokio::time::timeout(budget, handler(fired.clone())).await {
                Ok(result) => result,
                Err(_) => Err(format!("the handler ran past {}s", budget.as_secs())),
            }
        } else if claimed.owner == KERNEL_OWNER {
            Err(format!(
                "no handler for kernel timer '{}' on this server",
                claimed.name
            ))
        } else {
            self.dispatch_plugin(&fired).await
        };
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match &outcome {
            Ok(()) => info!(
                owner = %claimed.owner,
                name = %claimed.name,
                run_at = %claimed.run_at,
                fired_at = %claimed.fired_at,
                lateness_ms = claimed.lateness_ms,
                attempt = claimed.attempts,
                elapsed_ms,
                instance = %self.instance,
                "timer fired"
            ),
            Err(error) => warn!(
                owner = %claimed.owner,
                name = %claimed.name,
                attempt = claimed.attempts,
                lateness_ms = claimed.lateness_ms,
                elapsed_ms,
                error = %error,
                "timer failed"
            ),
        }
        if let Err(e) = self.settle(&claimed, outcome).await {
            warn!(owner = %claimed.owner, name = %claimed.name, error = %e,
                "could not record a timer's outcome; its lease returns it");
        }
    }

    /// Dispatch `tap_timer` to the owning plugin only.
    async fn dispatch_plugin(&self, fired: &FiredTimer) -> Result<(), String> {
        let Some(cron) = &self.cron else {
            return Err("no plugin dispatcher on this server".to_string());
        };
        let input = serde_json::json!({
            "name": fired.name,
            "payload": fired.payload,
            "run_at": fired.run_at.timestamp(),
            "fired_at": fired.fired_at.timestamp(),
            "repeat_secs": fired.repeat_secs,
            "attempt": fired.attempt,
        })
        .to_string();
        match cron
            .dispatch_background_tap("tap_timer", &fired.owner, &input)
            .await
        {
            None => Err("no plugin dispatcher on this server".to_string()),
            Some(DispatchOutcome::Ok(result)) => {
                // An object with an `error` string is a failure the plugin
                // reported without trapping.
                match serde_json::from_str::<serde_json::Value>(&result.output)
                    .ok()
                    .as_ref()
                    .and_then(|v| v.get("error"))
                    .and_then(|e| e.as_str())
                {
                    Some(error) => Err(error.to_string()),
                    None => Ok(()),
                }
            }
            Some(DispatchOutcome::NoHandler) => Err(format!(
                "plugin '{}' is not enabled or does not implement tap_timer",
                fired.owner
            )),
            Some(DispatchOutcome::CpuExhausted) => {
                Err("tap_timer used its whole CPU budget and was cut off".to_string())
            }
            Some(DispatchOutcome::WallClockExceeded) => {
                Err("tap_timer ran past its wall clock ceiling and was cut off".to_string())
            }
            Some(DispatchOutcome::Failed) => {
                Err("tap_timer failed (trap or error result)".to_string())
            }
        }
    }

    /// Record an attempt's outcome, only if the claim is still ours: a timer
    /// registered again, paused or cancelled while it ran is left as that
    /// change left it.
    async fn settle(&self, claimed: &Claimed, outcome: Result<(), String>) -> Result<()> {
        let outcome = outcome.map_err(|e| clip(&e, MAX_ERROR_BYTES));
        let mut tx = self.pool.begin().await?;
        let next: Option<DateTime<Utc>> = match outcome {
            Ok(()) if claimed.repeat_secs.is_none() => {
                sqlx::query(
                    "DELETE FROM scheduled_timer \
                     WHERE id = $1 AND status = 'running' AND claim_token = $2",
                )
                .bind(claimed.id)
                .bind(claimed.claim_token)
                .execute(&mut *tx)
                .await?;
                None
            }
            // The next slot after now, in whole intervals from this one. A
            // timer run before its slot (Run now on a repeating timer whose
            // first slot is still ahead) keeps that slot: never a negative
            // count of intervals, which would schedule it before its start.
            Ok(()) => sqlx::query_scalar(
                "UPDATE scheduled_timer SET status = 'scheduled', \
                     run_at = slot_at + make_interval(secs => repeat_secs * \
                         GREATEST(floor(EXTRACT(EPOCH FROM (clock_timestamp() - slot_at)) / repeat_secs) + 1, 0)), \
                     slot_at = slot_at + make_interval(secs => repeat_secs * \
                         GREATEST(floor(EXTRACT(EPOCH FROM (clock_timestamp() - slot_at)) / repeat_secs) + 1, 0)), \
                     attempts = 0, last_error = NULL, claim_token = NULL, claimed_by = NULL, \
                     claimed_until = NULL, changed = now() \
                 WHERE id = $1 AND status = 'running' AND claim_token = $2 \
                 RETURNING run_at",
            )
            .bind(claimed.id)
            .bind(claimed.claim_token)
            .fetch_optional(&mut *tx)
            .await?,
            Err(error) if claimed.attempts >= MAX_ATTEMPTS => {
                sqlx::query(
                    "UPDATE scheduled_timer SET status = 'failed', last_error = $3, \
                     claim_token = NULL, claimed_by = NULL, claimed_until = NULL, changed = now() \
                     WHERE id = $1 AND status = 'running' AND claim_token = $2",
                )
                .bind(claimed.id)
                .bind(claimed.claim_token)
                .bind(&error)
                .execute(&mut *tx)
                .await?;
                None
            }
            Err(error) => {
                let (base, cap) = self.retry.read().map(|r| *r).unwrap_or((RETRY_BASE_SECS, RETRY_CAP_SECS));
                let backoff = crate::cron::exponential_backoff_secs(base, cap, claimed.attempts);
                sqlx::query_scalar(
                    "UPDATE scheduled_timer SET status = 'scheduled', last_error = $3, \
                     run_at = clock_timestamp() + make_interval(secs => $4), \
                     claim_token = NULL, claimed_by = NULL, claimed_until = NULL, changed = now() \
                     WHERE id = $1 AND status = 'running' AND claim_token = $2 \
                     RETURNING run_at",
                )
                .bind(claimed.id)
                .bind(claimed.claim_token)
                .bind(&error)
                .bind(backoff as f64)
                .fetch_optional(&mut *tx)
                .await?
            }
        };
        if let Some(at) = next {
            notify(&mut tx, at).await?;
        }
        tx.commit().await?;
        if let Some(at) = next {
            self.wake_if_before(at.timestamp_millis());
        }
        Ok(())
    }

    /// Give back claims this server will not run (it is shutting down): each
    /// timer is scheduled again at the time it was due, its attempt not
    /// counted, and the other servers are told.
    async fn release(&self, claimed: &[Claimed]) {
        if claimed.is_empty() {
            return;
        }
        let ids: Vec<i64> = claimed.iter().map(|c| c.id).collect();
        let tokens: Vec<Uuid> = claimed.iter().map(|c| c.claim_token).collect();
        let released = async {
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                "UPDATE scheduled_timer t SET status = 'scheduled', \
                 attempts = GREATEST(t.attempts - 1, 0), claim_token = NULL, \
                 claimed_by = NULL, claimed_until = NULL, changed = now() \
                 FROM unnest($1::bigint[], $2::uuid[]) AS c(id, token) \
                 WHERE t.id = c.id AND t.status = 'running' AND t.claim_token = c.token",
            )
            .bind(&ids)
            .bind(&tokens)
            .execute(&mut *tx)
            .await?;
            if let Some(at) = claimed.iter().map(|c| c.run_at).min() {
                notify(&mut tx, at).await?;
            }
            tx.commit().await?;
            Ok::<_, anyhow::Error>(())
        };
        match released.await {
            Ok(()) => info!(
                count = ids.len(),
                "timer scheduler stopping: claims given back"
            ),
            Err(e) => {
                warn!(error = %e, "timer scheduler could not give back its claims; their lease returns them");
            }
        }
    }

    /// The scheduler loop: one per process, until `shutdown`.
    ///
    /// Sleeps until the next timer is due (at most [`SAFETY_RECHECK`]), fires
    /// what is due, and wakes early for a write that moves a timer before its
    /// wake time. From `shutdown` on it claims nothing; it returns once the
    /// timers already firing are done, so the caller bounds that wait by its
    /// shutdown deadline.
    pub async fn run_scheduler(self: Arc<Self>, shutdown: CancellationToken) {
        let listener = self.listen.load(Ordering::SeqCst).then(|| {
            tokio::spawn(listen(
                self.pool.clone(),
                self.notify.clone(),
                self.wake_at_ms.clone(),
                shutdown.clone(),
            ))
        });
        if let Ok(mut loops) = LOCAL_LOOPS.lock() {
            loops.push(LocalLoop {
                notify: Arc::downgrade(&self.notify),
                wake_at_ms: Arc::downgrade(&self.wake_at_ms),
            });
        }
        let mut in_flight: JoinSet<()> = JoinSet::new();
        // The concurrency throttle and when it was read: reread at most every
        // [`SETTINGS_REREAD`], so a change applies within it even while a
        // backlog keeps timers due without a break.
        let mut max_concurrent: Option<(u32, Instant)> = None;
        info!(instance = %self.instance, "Timer scheduler started");
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            // Looking now: any write from here on wakes the next sleep.
            self.wake_at_ms.store(i64::MAX, Ordering::SeqCst);
            while in_flight.try_join_next().is_some() {}

            let delay = match self.next_due_in_ms().await {
                Ok(Some(ms)) => Some(ms),
                Ok(None) => None,
                Err(e) => {
                    warn!(error = %e, "timer scheduler could not read the next timer");
                    Some(SAFETY_RECHECK.as_millis() as i64)
                }
            };

            let mut sleep_ms = delay
                .map_or(SAFETY_RECHECK.as_millis() as i64, |ms| ms.max(0))
                .min(SAFETY_RECHECK.as_millis() as i64);
            if delay.is_some_and(|ms| ms <= 0) {
                if max_concurrent.is_none_or(|(_, read)| read.elapsed() >= SETTINGS_REREAD) {
                    let cap = self
                        .settings()
                        .await
                        .map(|s| s.max_concurrent.max(1))
                        .unwrap_or(TimerSettings::default().max_concurrent);
                    max_concurrent = Some((cap, Instant::now()));
                }
                let cap = max_concurrent.map_or(1, |(cap, _)| cap) as usize;
                let room = cap.saturating_sub(in_flight.len());
                if room > 0 {
                    let hook = self.before_claim.read().ok().and_then(|h| h.clone());
                    if let Some(hook) = hook {
                        hook();
                    }
                    match self.claim(room as i64).await {
                        // Shutdown came while this server was reading or
                        // claiming: start nothing, and give the claims back
                        // so another server fires them now rather than
                        // after the lease.
                        Ok(claimed) if shutdown.is_cancelled() => {
                            self.release(&claimed).await;
                            break;
                        }
                        Ok(claimed) => {
                            let took = claimed.len();
                            for c in claimed {
                                in_flight.spawn(self.clone().fire(c));
                            }
                            if took == room {
                                // More may be due: look again at once.
                                continue;
                            }
                            // Due rows another server is claiming right now
                            // were skipped; look again shortly.
                            sleep_ms = 50;
                        }
                        Err(e) => {
                            warn!(error = %e, "timer scheduler could not claim due timers");
                            sleep_ms = 1000;
                        }
                    }
                } else {
                    // Full: wait for a timer to finish (or the safety recheck).
                    sleep_ms = SAFETY_RECHECK.as_millis() as i64;
                }
            } else {
                // Reread the throttles next time something is due.
                max_concurrent = None;
            }

            let now_ms = Utc::now().timestamp_millis();
            self.wake_at_ms
                .store(now_ms.saturating_add(sleep_ms), Ordering::SeqCst);
            let sleep = tokio::time::sleep(Duration::from_millis(sleep_ms.max(0) as u64));
            tokio::select! {
                () = sleep => {}
                () = self.notify.notified() => {}
                () = shutdown.cancelled() => break,
                Some(_) = in_flight.join_next(), if !in_flight.is_empty() => {}
            }
        }
        // No timer starts from here; the ones firing finish.
        self.wake_at_ms.store(i64::MAX, Ordering::SeqCst);
        if !in_flight.is_empty() {
            info!(
                in_flight = in_flight.len(),
                "Timer scheduler stopping; waiting for the timers firing"
            );
        }
        while in_flight.join_next().await.is_some() {}
        if let Some(listener) = listener {
            listener.abort();
        }
        info!(instance = %self.instance, "Timer scheduler stopped");
    }
}

/// Listen for every server's timer writes and wake this server's loop for one
/// due before it would look. Reconnects after a lost connection; the loop's
/// safety recheck covers what is missed meanwhile.
async fn listen(
    pool: PgPool,
    notify: Arc<Notify>,
    wake_at_ms: Arc<AtomicI64>,
    shutdown: CancellationToken,
) {
    loop {
        let connected = async {
            let mut listener = sqlx::postgres::PgListener::connect_with(&pool).await?;
            listener.listen(NOTIFY_CHANNEL).await?;
            Ok::<_, sqlx::Error>(listener)
        };
        let mut listener = tokio::select! {
            () = shutdown.cancelled() => return,
            result = connected => match result {
                Ok(listener) => listener,
                Err(e) => {
                    warn!(error = %e, "timer listener could not connect; retrying");
                    tokio::select! {
                        () = shutdown.cancelled() => return,
                        () = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    }
                }
            },
        };
        // Whatever was written while the listener was away.
        notify.notify_one();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                received = listener.recv() => match received {
                    Ok(notification) => {
                        let at = notification.payload().parse::<i64>().unwrap_or(i64::MIN);
                        if at < wake_at_ms.load(Ordering::SeqCst) {
                            notify.notify_one();
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "timer listener lost its connection; reconnecting");
                        break;
                    }
                },
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_override_has_a_ceiling() {
        let at = |v: &'static str| {
            TimerOverrides::from_lookup(move |name| {
                (name == "TIMER_MAX_PAYLOAD_BYTES").then(|| v.to_string())
            })
        };
        assert_eq!(
            at("65536").unwrap().max_payload_bytes,
            Some(MAX_PAYLOAD_LIMIT)
        );
        assert!(at("65537").is_err(), "past what a plugin can read back");
    }

    #[test]
    fn a_long_error_is_clipped_on_a_character_boundary() {
        assert_eq!(clip("short", 10), "short");
        let long = "é".repeat(10);
        let clipped = clip(&long, 5);
        assert_eq!(clipped, "éé", "4 bytes: the third é would split");
    }

    #[test]
    fn overrides_read_whole_numbers_and_refuse_the_rest() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        assert_eq!(
            TimerOverrides::from_lookup(env(&[])).unwrap(),
            TimerOverrides::default()
        );
        let o = TimerOverrides::from_lookup(env(&[
            ("TIMER_MAX_CONCURRENT", " 8 "),
            ("TIMER_MIN_REPEAT_SECS", ""),
        ]))
        .unwrap();
        assert_eq!(o.max_concurrent, Some(8));
        assert_eq!(o.min_repeat_secs, None);
        assert_eq!(o.apply(TimerSettings::default()).max_concurrent, 8);
        assert!(TimerOverrides::from_lookup(env(&[("TIMER_MAX_PER_PLUGIN", "0")])).is_err());
        assert!(TimerOverrides::from_lookup(env(&[("TIMER_MAX_PAYLOAD_BYTES", "lots")])).is_err());
    }

    #[test]
    fn names_and_repeats_are_checked() {
        assert!(valid_name("publish:1"));
        assert!(!valid_name(""));
        assert!(!valid_name("a\nb"));
        assert!(!valid_name(&"x".repeat(MAX_NAME_BYTES + 1)));
        let s = TimerSettings::default();
        assert_eq!(check_repeat(None, &s).unwrap(), None);
        assert_eq!(check_repeat(Some(60), &s).unwrap(), Some(60));
        assert!(matches!(
            check_repeat(Some(59), &s),
            Err(TimerRefused::RepeatTooShort { min: 60 })
        ));
        assert!(check_repeat(Some(0), &s).is_err());
        assert!(check_repeat(Some(-5), &s).is_err());
    }

    #[test]
    fn statuses_round_trip() {
        for status in TimerStatus::ALL {
            assert_eq!(TimerStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(TimerStatus::parse("done"), None);
    }
}
