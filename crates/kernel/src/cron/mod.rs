//! Scheduled operations and background tasks.
//!
//! Provides distributed cron with Redis-based locking to ensure
//! exactly-once execution across multiple server instances.

mod pagefind;
mod queue;
mod search_index;
mod search_update;
mod tasks;

/// The Pagefind export seam, for tests that assert which items and which text
/// reach the static index without a Pagefind CLI on the path. The module itself
/// is private.
#[doc(hidden)]
pub use pagefind::{
    IndexDocument, IndexedField, ItemDocuments, indexable_documents, indexable_documents_for,
    maybe_rebuild_index_with, render_fragment, request_rebuild as request_search_index_rebuild,
};
/// The native search index build, for the gate tests: build an index from a
/// given set of documents through the ledger and publish it.
#[doc(hidden)]
pub use search_index::{
    FullBuildOptions, FullBuildReport, RETENTION_HOURS, RebuildAgain,
    full_build as search_index_full_build, full_build_with as search_index_full_build_with,
    generation as search_index_generation,
};
/// The incremental search index update and the cron run over it, for the gate
/// tests.
#[doc(hidden)]
pub use search_update::{
    BATCH_SIZE as SEARCH_INDEX_BATCH_SIZE, DEFAULT_COMPACTION_PERCENT, DocumentSource, RunOptions,
    RunReport, UpdateOutcome, UpdateReport, incremental_update as search_index_incremental_update,
    run_index as search_index_run,
};

pub use tasks::UpdateCheckConfig;

pub use queue::{Queue, RedisQueue};
pub use tasks::CronTasks;

use crate::redis_conn::BoundedRedis;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use anyhow::{Context, Result};
use redis::{AsyncCommands, Client as RedisClient};
use sqlx::PgPool;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::content::ItemService;
use crate::file::FileService;
use crate::gather::GatherService;
use crate::services::ai_provider::AiProviderService;
use crate::services::ai_token_budget::AiTokenBudgetService;
use crate::services::embed_index::{
    self, EmbedJobPayload, KERNEL_EMBED_CONCURRENCY, KERNEL_EMBED_PLUGIN,
};
use crate::services::vector_store::{PgVectorStore, VectorStore};
use crate::tap::{DispatchOutcome, RequestState, TapDispatcher};

/// Lock TTL in seconds (5 minutes).
const LOCK_TTL_SECS: u64 = 300;

/// Heartbeat interval in seconds (60 seconds).
const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// Cron lock key in Redis.
const CRON_LOCK_KEY: &str = "cron:lock";

/// Redis key holding the last completed run ([`LastCronRun`]).
const CRON_LAST_RUN_KEY: &str = "cron:last_run";

/// Maximum items processed per plugin per drain cycle (P11d fairness cap): one
/// plugin flooding its queue cannot starve another. Ratified value retained.
const MAX_QUEUE_ITEMS_PER_CYCLE: i64 = 100;

/// Kernel ceiling on honored per-plugin worker concurrency (P11d / D-47). A
/// plugin's declared `tap_queue_info.concurrency` is clamped to this.
const QUEUE_CONCURRENCY_CAP: usize = 4;

/// Claim lease in seconds: a claimed-but-unfinished job stays locked this long
/// before it is reclaimable (crash recovery / at-least-once). Matches the cron
/// lock TTL so a claim outlives a single cron cycle.
const QUEUE_CLAIM_LEASE_SECS: i64 = 300;

/// Backoff base (seconds) for the first retry; doubles each subsequent attempt.
const QUEUE_BACKOFF_BASE_SECS: i64 = 60;

/// Backoff ceiling (seconds).
const QUEUE_BACKOFF_CAP_SECS: i64 = 3600;

/// `site_config` key gating the opt-in resident queue-runner (P11d / D-46).
const QUEUE_RUNNER_CONFIG_KEY: &str = "queue_runner";

/// Wall-clock budget for one `drain_plugin_queues` pass, checked between
/// batches.
///
/// The drain had no bound of its own. Its only limit was
/// `MAX_QUEUE_ITEMS_PER_CYCLE` jobs per plugin, and a job may legitimately
/// occupy a worker for the whole background epoch deadline, so one pass could
/// run for `100 / width * deadline` seconds: over an hour at the shipped values.
/// It ran inside a cron run holding the global cron lock, so for that whole
/// time every other poke answered "another instance is running cron", `tap_cron`
/// never dispatched, and nothing that lives in `tap_cron` (a plugin's own
/// stuck-queue alarm, for instance) could observe or report the condition.
///
/// This bounds how many more batches a pass will start, not the batch already in
/// flight, so the true ceiling on one pass is this plus one dispatch. Work left
/// over is not lost: it is claimable again on the next cycle.
const QUEUE_DRAIN_BUDGET_SECS: u64 = 60;

/// Longest a single cron run may keep renewing the cron lock.
///
/// [`run_heartbeat`] extended the lock every `LOCK_TTL_SECS / 2` for as long as
/// the run had not returned, with no condition attached. A run that could not
/// finish therefore kept the lock healthy-looking forever: `TTL cron:lock` sat
/// at its maximum and rose between samples precisely *because* the run was
/// stuck, and the outage lasted until someone restarted the process.
///
/// Renewal now stops here. The lock then expires on its own TTL, the next cron
/// trigger proceeds, and whatever reports on queue health gets to run again.
/// Deliberately longer than a legitimate long run needs, so this is a backstop
/// against an unforeseen stall rather than a routine bound: the routine bound is
/// [`QUEUE_DRAIN_BUDGET_SECS`].
const MAX_LOCK_RENEWAL_SECS: u64 = 900;

/// Longest one ordinary cron task may run before the run gives up on it and
/// moves on to the next (B247).
///
/// The tasks had no bound of their own. A task waiting on a Redis or a database
/// that had stopped answering waited for ever, and the run with it: in the
/// incident behind this, one run sat in `process_queues` for ten hours while its
/// heartbeat kept the lock alive and every later tick was skipped. Every task
/// the run starts is now cut off here, logged at warn, recorded as
/// `<task>:TIMEOUT` the way `tap_cron` already was, and the run carries on and
/// releases the lock. Half the lock TTL, the same allowance `tap_cron` has
/// always had: an ordinary task is a bounded batch of deletes or a few queue
/// pops and finishes in milliseconds to seconds, so this is a backstop, not a
/// budget anyone is expected to use.
const CRON_TASK_TIMEOUT_SECS: u64 = LOCK_TTL_SECS / 2;

/// Marks where a cron lock value records when its run took the lock.
///
/// The value is `host:pid:uuid:t<unix seconds>`. The owner check compares the whole
/// value, so the time costs nothing there; it is what lets a run that finds the
/// lock held say how long the holder has had it. A value without the marker
/// (written by an older kernel during a rolling deploy) still works as a lock,
/// it just has no age to report.
const LOCK_SINCE_MARKER: &str = ":t";

/// Slack allowed when deciding that a failed dispatch ran to its epoch deadline.
///
/// The engine's epoch advances once per second, so a deadline of N ticks can
/// expire anywhere in `(N-1, N]` seconds of wall clock: the uncertainty is one
/// tick, an **absolute** second, not a fraction of the budget. A proportional
/// tolerance gets this wrong at small budgets — at a 5-second budget, 10% is half
/// a second against a full second of granularity — which is how a job that had
/// plainly run to its deadline was read as an ordinary failure and rescheduled.
/// One tick plus scheduling slack covers it at every budget size. At the shipped
/// 150-second budget this still leaves the boundary at 148 seconds, three orders
/// of magnitude away from the milliseconds an ordinary trap takes, so widening it
/// costs no precision where it matters.
const CPU_EXHAUSTION_SLACK: Duration = Duration::from_secs(2);

/// Smallest epoch budget for which exhaustion can be told apart from an ordinary
/// failure at all.
///
/// Below this, [`CPU_EXHAUSTION_SLACK`] would swallow the whole budget and every
/// failure would read as exhaustion. Such a budget is a misconfiguration; the
/// classification declines rather than dead-lettering everything.
const MIN_CLASSIFIABLE_BUDGET_SECS: u64 = 3;

/// `dead_reason` for a job retired by [`CronService::reap_exhausted_jobs`]: it
/// reached `max_attempts` while claimed and its lease expired without any
/// terminal outcome being recorded, so no failure path ever ran for it.
const ABANDONED_CLAIM_REASON: &str =
    "abandoned at max_attempts: claim lease expired with no terminal outcome recorded";

/// Result of a cron run.
#[derive(Debug, Clone)]
pub enum CronResult {
    /// Cron ran successfully.
    Completed {
        /// Tasks executed.
        tasks_run: Vec<String>,
        /// Duration of the run.
        duration_ms: u64,
    },
    /// Another instance is already running.
    Skipped,
    /// Cron failed with an error.
    Failed(String),
}

/// A queue job claimed for one worker dispatch (P11d).
///
/// `Clone` so the drain can keep a record of what it dispatched: if it later
/// gives up waiting on a job, the row still has to be handed back to the queue,
/// and by then the task that owned the original has been aborted.
#[derive(Debug, Clone)]
struct ClaimedJob {
    /// `plugin_queue.id` of the claimed row.
    id: i64,
    /// The job payload passed to `tap_queue_worker`.
    payload: serde_json::Value,
    /// Attempts made *including* this claim (incremented at claim time).
    attempts: i32,
    /// Attempt bound after which the job is dead-lettered.
    max_attempts: i32,
}

/// How far past a worker's own CPU budget the drain waits before it stops
/// waiting on an in-flight job.
///
/// The epoch deadline is the bound a *well-behaved* worker answers to: a guest
/// that keeps executing is interrupted there. It cannot bound a guest that is
/// not executing — one parked in a host call that never comes back — so the
/// drain needs a bound of its own, and it has to sit above the epoch deadline
/// or it would abandon jobs the epoch mechanism was about to cut off cleanly.
/// One epoch tick of slack past the budget is enough to tell the two apart.
const JOB_ABANDON_SLACK: Duration = Duration::from_secs(30);

/// Terminal outcome of a single queue job dispatch (P11d drain stats).
#[derive(Debug, Clone, Copy)]
enum JobOutcome {
    /// Worker returned; the row was deleted.
    Succeeded,
    /// Worker failed and the row was rescheduled with backoff.
    Retried,
    /// Worker failed at `max_attempts`; the row was dead-lettered.
    DeadLettered,
}

impl JobOutcome {
    /// Stable name for the outcome, for the per-job log line.
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Retried => "retried",
            Self::DeadLettered => "dead_lettered",
        }
    }
}

/// Aggregate outcome of a [`CronService::drain_plugin_queues`] pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueueDrainStats {
    /// Jobs whose worker succeeded (deleted).
    pub succeeded: u64,
    /// Jobs rescheduled with backoff after a failed attempt.
    pub retried: u64,
    /// Jobs dead-lettered at `max_attempts`.
    pub dead_lettered: u64,
    /// Bookkeeping failures (DB update errors / worker-task panics).
    pub errors: u64,
}

impl QueueDrainStats {
    /// Fold one job's terminal outcome into the running totals.
    fn record(&mut self, outcome: JobOutcome) {
        match outcome {
            JobOutcome::Succeeded => self.succeeded += 1,
            JobOutcome::Retried => self.retried += 1,
            JobOutcome::DeadLettered => self.dead_lettered += 1,
        }
    }

    /// Jobs that reached a terminal state (succeeded + retried + dead-lettered).
    pub fn total(&self) -> u64 {
        self.succeeded + self.retried + self.dead_lettered
    }
}

/// Resident queue-runner configuration (P11d / D-46 hybrid, opt-in).
///
/// Read from the `queue_runner` `site_config` key. Absent or `enabled = false`
/// (the default) drains the queue on the cron cadence. When
/// `enabled = true`, a boot-started background task drains the plugin queue on
/// its own cadence (`poll_interval_secs`) and the in-request cron drain steps
/// aside to avoid double-draining.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct QueueRunnerConfig {
    /// Whether the resident runner is active. Default `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Seconds between drain passes. Default 5; floored at 1 at use.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

/// Default resident-runner poll interval (seconds).
fn default_poll_interval() -> u64 {
    5
}

impl Default for QueueRunnerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: default_poll_interval(),
        }
    }
}

/// `site_config` key holding the built-in scheduler's interval, in seconds, as
/// a bare JSON number (B178). Edited on the cron report (`/admin/reports/cron`).
pub const CRON_INTERVAL_CONFIG_KEY: &str = "cron_interval_secs";

/// Default built-in scheduler interval, seconds: five minutes, the cadence the
/// stock sidecar pokers used, and equal to the cron lock TTL, so one lock
/// comfortably covers a run end to end.
///
/// Non-zero on purpose: core has to be fully functional with nothing external
/// configured, so a freshly installed site publishes scheduled content,
/// rebuilds its search index and cleans up after itself on its own. A `0`
/// default would leave a fresh install exactly as silent as it was before the
/// scheduler existed. Sites that already run an external caller are unaffected
/// either way: `/cron/{key}` keeps answering, and the lock in
/// [`CronService::run`] makes a scheduled run and an external poke that land in
/// the same window skip one another rather than double-run.
pub const DEFAULT_CRON_INTERVAL_SECS: u64 = 300;

/// The longest the scheduler loop sleeps before rereading its setting, so a
/// change (an interval shortened from an hour to a minute, or the scheduler
/// turned back on) applies within a minute without a restart.
const SCHEDULER_MAX_POLL_SECS: u64 = 60;

/// Where the built-in scheduler's interval came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CronIntervalSource {
    /// No setting saved: [`DEFAULT_CRON_INTERVAL_SECS`].
    Default,
    /// The `cron_interval_secs` `site_config` row.
    SiteConfig,
    /// The `CRON_INTERVAL_SECS` environment variable, which wins outright.
    Environment,
}

/// Built-in scheduler configuration (B178).
///
/// `interval_secs == 0` turns the built-in scheduler off: cron then runs on
/// nothing but a request to `/cron/{key}` or the admin "Run now" control,
/// exactly as before the scheduler existed. Any other value is how often
/// [`CronService::run_scheduler`] calls `run` on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CronSchedulerConfig {
    /// Seconds between scheduled runs; `0` is off.
    pub interval_secs: u64,
    /// Where `interval_secs` came from.
    pub source: CronIntervalSource,
}

impl Default for CronSchedulerConfig {
    fn default() -> Self {
        Self {
            interval_secs: DEFAULT_CRON_INTERVAL_SECS,
            source: CronIntervalSource::Default,
        }
    }
}

impl CronSchedulerConfig {
    /// Whether the built-in scheduler calls `run` at all.
    pub fn enabled(&self) -> bool {
        self.interval_secs > 0
    }

    /// Read the interval out of a stored `site_config` value: a JSON number,
    /// or a string holding one. Anything else is `None`, and the caller falls
    /// back to the default rather than to off.
    pub fn interval_from_value(value: &serde_json::Value) -> Option<u64> {
        match value {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

/// Whether a scheduled run is due, given when the last run anywhere finished.
///
/// `last_finished` is the `cron:last_run` timestamp, written by whichever
/// caller ran last: this server's scheduler, another server's, `/cron/{key}` or
/// the admin button. A run that finished less than `interval_secs` ago means
/// cron has already run in this window, so the tick is skipped. That is what
/// keeps several kernel instances, each with its own scheduler, from running
/// cron once per instance per interval: together they run it about once per
/// interval. Both timestamps are whole seconds, and the run's finish is
/// floored, so a run that finished exactly one interval ago counts as due.
pub(crate) fn scheduled_run_due(now: i64, last_finished: Option<i64>, interval_secs: u64) -> bool {
    match last_finished {
        None => true,
        Some(finished) => now.saturating_sub(finished) >= interval_secs as i64,
    }
}

/// The setting the scheduler loop acts on after a read.
///
/// A successful read wins. A failed one keeps the setting last read, so a
/// database error cannot turn on a scheduler an administrator turned off,
/// nor change its interval; only a failure before anything was ever read
/// falls back to the default.
pub(crate) fn next_scheduler_config(
    known: Option<CronSchedulerConfig>,
    read: Result<CronSchedulerConfig>,
) -> CronSchedulerConfig {
    match (read, known) {
        (Ok(cfg), _) => cfg,
        (Err(_), Some(cfg)) => cfg,
        (Err(_), None) => CronSchedulerConfig::default(),
    }
}

/// Exponential backoff for a failed attempt: `BASE * 2^(attempts-1)`, capped at
/// [`QUEUE_BACKOFF_CAP_SECS`]. `attempts` is the post-claim count (>= 1).
fn backoff_secs(attempts: i32) -> i64 {
    exponential_backoff_secs(QUEUE_BACKOFF_BASE_SECS, QUEUE_BACKOFF_CAP_SECS, attempts)
}

/// `base * 2^(attempts-1)`, capped at `cap`. `attempts` is the post-claim count
/// (>= 1). Shared by the plugin queue and the mail outbox (BG18), which adds
/// jitter on top.
pub(crate) fn exponential_backoff_secs(base: i64, cap: i64, attempts: i32) -> i64 {
    let exp = attempts.saturating_sub(1).clamp(0, 16) as u32;
    base.saturating_mul(1i64 << exp).min(cap)
}

/// Read the declared `concurrency` of each queue from a `tap_queue_info` result.
///
/// `tap_queue_info` returns a JSON array of
/// `{ "name": string, "concurrency": int }`. Each entry is a statement about
/// **that queue**, so the result is a map keyed by queue name rather than a
/// single number. Entries without a usable `name` are dropped (they name no
/// queue, so they bound nothing); an entry whose `concurrency` is missing,
/// unparseable or zero falls back to 1. Every width is clamped to
/// [`QUEUE_CONCURRENCY_CAP`], so a declaration can lower the kernel ceiling but
/// never raise it. A duplicate name keeps the lower of the two, which is the
/// safe reading of a contradictory declaration.
fn parse_queue_concurrency(output: &str) -> BTreeMap<String, usize> {
    let mut widths = BTreeMap::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return widths;
    };
    let Some(queues) = value.as_array() else {
        return widths;
    };
    for q in queues {
        let Some(name) = q.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let width = q
            .get("concurrency")
            .and_then(serde_json::Value::as_u64)
            .map_or(1, |c| c as usize)
            .clamp(1, QUEUE_CONCURRENCY_CAP);
        widths
            .entry(name.to_string())
            .and_modify(|existing: &mut usize| *existing = (*existing).min(width))
            .or_insert(width);
    }
    widths
}

/// Dispatch `tap_queue_worker` for one claimed job and record its terminal
/// outcome (P11d). Runs as an independent task so a plugin's declared
/// concurrency actually executes in parallel.
///
/// Success (`tap_queue_worker` returns) deletes the row. Failure (the worker
/// traps or returns an error result, surfaced as `None` from
/// `dispatch_to_plugin`) either reschedules with backoff or, at
/// `max_attempts`, dead-letters the row with the last error preserved.
///
/// A guest the dispatcher reports was cut off at the epoch deadline is treated
/// differently: it is dead-lettered at once, whatever `attempts` says. See
/// [`mark_job_cpu_exhausted`].
async fn run_queue_job(
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    services: crate::tap::RequestServices,
    plugin_name: String,
    queue_name: String,
    job: ClaimedJob,
    invocation_sink: Arc<AtomicU64>,
) -> Result<JobOutcome> {
    // Gander's queue choke point: a job is traced like a request. It has no
    // request profile: nothing would read its counts.
    let span = tracing::trace_span!(
        target: crate::telemetry::TARGET,
        "queue.job",
        trovato.plugin = %plugin_name,
        trovato.queue = %queue_name,
        trovato.queue.attempt = job.attempts,
        trovato.queue.outcome = tracing::field::Empty,
    );
    let outcome = tracing::Instrument::instrument(
        run_queue_job_inner(
            pool,
            dispatcher,
            services,
            plugin_name,
            queue_name,
            job,
            invocation_sink,
        ),
        span.clone(),
    )
    .await;
    if let Ok(o) = outcome.as_ref() {
        span.record("trovato.queue.outcome", o.as_str());
    }
    outcome
}

async fn run_queue_job_inner(
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    services: crate::tap::RequestServices,
    plugin_name: String,
    queue_name: String,
    job: ClaimedJob,
    invocation_sink: Arc<AtomicU64>,
) -> Result<JobOutcome> {
    // Infallible: payload came from JSONB and round-trips to a string.
    let input_json = serde_json::to_string(&job.payload).unwrap_or_else(|_| "{}".to_string());

    // P11c / D-40: queue-worker dispatch carries the kernel-internal background
    // principal so a plugin holding `ai_background` may call AI from the worker.
    // `services` comes from `CronService::background_services`, the builder
    // `tap_cron` uses too, so the rate limiter and the email service reach a
    // queue worker exactly as they reach cron.
    let state = RequestState::new(crate::tap::UserContext::background(), services)
        .with_invocation_sink(invocation_sink);

    let budget = dispatcher
        .runtime()
        .limits()
        .background_tap_epoch_deadline_secs;
    let started = std::time::Instant::now();
    let outcome = dispatcher
        .dispatch_to_plugin_outcome("tap_queue_worker", &input_json, &plugin_name, state)
        .await;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let settled = match outcome {
        DispatchOutcome::Ok(_) => {
            mark_job_succeeded(&pool, job.id).await?;
            Ok(JobOutcome::Succeeded)
        }
        // The guest used its whole budget of *guest* CPU and was cut off.
        // Retrying buys another full budget burned against the same wall, so it
        // dies now. This is the dispatcher's own account of what happened, not
        // an inference from how long the call took.
        DispatchOutcome::CpuExhausted => {
            mark_job_cpu_exhausted(&pool, &job, &plugin_name, budget).await
        }
        // Ran past the wall clock ceiling, mostly waiting rather than
        // computing. Retrying buys another full ceiling against the same wall,
        // so it dies now for the same reason CPU exhaustion does — but it is
        // recorded as its own outcome, because the two say different things
        // about what the plugin was doing.
        DispatchOutcome::WallClockExceeded => {
            mark_job_wallclock_exceeded(&pool, &job, &plugin_name).await
        }
        DispatchOutcome::NoHandler | DispatchOutcome::Failed => {
            let err = "tap_queue_worker failed (trap or error result)";
            mark_job_failed(&pool, &job, &plugin_name, err).await
        }
    };

    // One line per job, whatever became of it. Before this, `elapsed` was
    // measured and thrown away on the success and ordinary-failure paths, and a
    // succeeded row is deleted — so nobody could answer "how long do jobs take"
    // without sampling the table from outside and hoping to catch one.
    if let Ok(outcome) = settled.as_ref() {
        info!(
            plugin = %plugin_name,
            queue = %queue_name,
            item_id = job.id,
            attempt = job.attempts,
            elapsed_ms = elapsed_ms,
            outcome = outcome.as_str(),
            "queue job finished"
        );
    }
    settled
}

/// Dead-letter a job that spent its entire CPU budget (P11d).
///
/// Unlike [`mark_job_failed`] this ignores `attempts`: the bound that matters is
/// the one the job already breached. Recording it as dead on the first
/// occurrence is what stops a worker that burns its budget from being handed the
/// same budget again on the next cycle, and the next, holding a worker slot each
/// time.
///
/// This is reached only when the dispatcher reports the guest was cut off at the
/// epoch deadline, which now counts guest execution and not time spent waiting
/// in host calls. It used to be reached by comparing elapsed wall clock against
/// the budget, on the reasoning that exhaustion and an ordinary trap were
/// "orders of magnitude apart". They are not: at the shipped 150 seconds the
/// margin over an observed analyze call of 12 to 17 seconds is about tenfold,
/// and a single slow provider response was enough to dead-letter a job on its
/// first attempt.
async fn mark_job_cpu_exhausted(
    pool: &PgPool,
    job: &ClaimedJob,
    plugin_name: &str,
    budget_secs: u64,
) -> Result<JobOutcome> {
    let now = chrono::Utc::now().timestamp();
    let reason =
        format!("tap_queue_worker exhausted its {budget_secs}s CPU budget and was cut off");
    sqlx::query(
        r#"
        UPDATE plugin_queue
        SET status = 'dead', dead_reason = $2, dead_at = $3,
            last_error = $2, locked_until = 0
        WHERE id = $1
        "#,
    )
    .bind(job.id)
    .bind(&reason)
    .bind(now)
    .execute(pool)
    .await
    .context("failed to dead-letter a CPU-exhausted queue item")?;
    warn!(
        plugin = %plugin_name,
        item_id = job.id,
        attempts = job.attempts,
        budget_secs = budget_secs,
        "queue item dead-lettered: it spent its whole CPU budget"
    );
    Ok(JobOutcome::DeadLettered)
}

/// Dead-letter a queue item whose worker ran past its wall clock ceiling.
///
/// Separate from [`mark_job_cpu_exhausted`] because the two describe different
/// runaways and the operator reading `dead_reason` has to tell them apart: one
/// job was computing and used its budget, this one was waiting in host calls
/// that each extended that budget. The disposition is the same — retrying buys
/// another full ceiling against the same wall — but the reason is not.
async fn mark_job_wallclock_exceeded(
    pool: &PgPool,
    job: &ClaimedJob,
    plugin_name: &str,
) -> Result<JobOutcome> {
    let now = chrono::Utc::now().timestamp();
    let reason = "tap_queue_worker ran past its wall clock ceiling and was cut off".to_string();
    sqlx::query(
        r#"
        UPDATE plugin_queue
        SET status = 'dead', dead_reason = $2, dead_at = $3,
            last_error = $2, locked_until = 0
        WHERE id = $1
        "#,
    )
    .bind(job.id)
    .bind(&reason)
    .bind(now)
    .execute(pool)
    .await
    .context("failed to dead-letter a wall-clock-exceeded queue item")?;
    warn!(
        plugin = %plugin_name,
        item_id = job.id,
        attempts = job.attempts,
        "queue item dead-lettered: it ran past its wall clock ceiling"
    );
    Ok(JobOutcome::DeadLettered)
}

/// Delete a queue row whose job reached a terminal *success* (P11d). Shared by
/// the plugin worker arm and the native embed arm (P11f) so both consume a
/// finished job identically.
async fn mark_job_succeeded(pool: &PgPool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM plugin_queue WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .context("failed to delete succeeded queue item")?;
    Ok(())
}

/// Apply the queue-v2 failure bookkeeping to a job whose dispatch failed
/// (P11d): reschedule with exponential backoff, or dead-letter (with the error
/// preserved) once `attempts` reaches `max_attempts`. Shared by the plugin
/// worker arm and the native embed arm (P11f), so a failed embed inherits the
/// identical retry→DLQ semantics rather than a bespoke path.
async fn mark_job_failed(
    pool: &PgPool,
    job: &ClaimedJob,
    plugin_name: &str,
    err: &str,
) -> Result<JobOutcome> {
    let now = chrono::Utc::now().timestamp();
    if job.attempts >= job.max_attempts {
        sqlx::query(
            r#"
            UPDATE plugin_queue
            SET status = 'dead', dead_reason = $2, dead_at = $3,
                last_error = $2, locked_until = 0
            WHERE id = $1
            "#,
        )
        .bind(job.id)
        .bind(err)
        .bind(now)
        .execute(pool)
        .await
        .context("failed to dead-letter queue item")?;
        warn!(
            plugin = %plugin_name,
            item_id = job.id,
            attempts = job.attempts,
            "queue item dead-lettered at max attempts"
        );
        Ok(JobOutcome::DeadLettered)
    } else {
        let backoff = backoff_secs(job.attempts);
        sqlx::query(
            r#"
            UPDATE plugin_queue
            SET status = 'ready', next_attempt_at = $2, last_error = $3, locked_until = 0
            WHERE id = $1
            "#,
        )
        .bind(job.id)
        .bind(now + backoff)
        .bind(err)
        .execute(pool)
        .await
        .context("failed to reschedule queue item")?;
        warn!(
            plugin = %plugin_name,
            item_id = job.id,
            attempts = job.attempts,
            backoff_secs = backoff,
            "queue item failed; rescheduled with backoff"
        );
        Ok(JobOutcome::Retried)
    }
}

/// Run one native kernel embed job (P11f / D-52).
///
/// The kernel-internal consumer arm: unlike [`run_queue_job`] it dispatches no
/// `tap_queue_worker` — it embeds the item natively and writes both the
/// `item_embeddings` vector (via the pgvector store) and the observable
/// `item_embed_status` (D-51). It shares the exact claim/backoff/DLQ bookkeeping
/// ([`mark_job_succeeded`] / [`mark_job_failed`]) so a failed embed retries and
/// eventually dead-letters like any other job.
///
/// Terminal outcomes:
/// - **Succeeded** (row deleted): the embedding landed; OR the job was
///   superseded by newer content (coalesced away); OR the item was deleted; OR
///   no provider / no vector backend is available (a graceful skip — the item
///   stays `pending`, backfill re-embeds later — *not* a failure, so it never
///   dead-letters for a config gap).
/// - **Retried / DeadLettered**: the embedding **provider call failed**
///   (transport/HTTP/parse) or the vector store write failed (DB). This is the
///   retry→DLQ path that replaces PF-4's silent drop; on dead-letter the item's
///   observable state flips to `failed` with the error.
///
/// Idempotent: the store upsert is keyed on `(item_id, field_name, model)`, so
/// a duplicate run (at-least-once delivery) re-writes the same row rather than
/// double-inserting or erroring.
async fn run_embed_job(
    pool: PgPool,
    ai_providers: Option<Arc<AiProviderService>>,
    vector_store: Option<Arc<PgVectorStore>>,
    job: ClaimedJob,
) -> Result<JobOutcome> {
    let payload: EmbedJobPayload = match serde_json::from_value(job.payload.clone()) {
        Ok(p) => p,
        Err(e) => {
            // A kernel-produced payload is always well-formed; a malformed row
            // is unrunnable garbage — discard it rather than retry forever.
            warn!(row_id = job.id, error = %e, "malformed embed job payload; discarding");
            mark_job_succeeded(&pool, job.id).await?;
            return Ok(JobOutcome::Succeeded);
        }
    };

    // Load the item fresh — the drain always embeds current content.
    let item = match crate::models::Item::find_by_id(&pool, payload.item_id).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            // Deleted since enqueue — nothing to embed.
            mark_job_succeeded(&pool, job.id).await?;
            return Ok(JobOutcome::Succeeded);
        }
        Err(e) => {
            // A DB read error is transient — retry/DLQ.
            return mark_job_failed(
                &pool,
                &job,
                KERNEL_EMBED_PLUGIN,
                &format!("load item for embed: {e}"),
            )
            .await;
        }
    };

    // Coalescing / stale-job detection: if the item's current embeddable text no
    // longer matches the hash captured at enqueue, a newer save superseded this
    // job. Skip it — the newer job carries the current hash and embeds the
    // latest content exactly once.
    let text = crate::content::item_service::item_embedding_text(&item);
    let current_hash = embed_index::embed_content_hash(&text);
    if embed_index::is_stale(&payload.content_hash, &current_hash) {
        debug!(item_id = %item.id, "embed job superseded by newer content; coalescing");
        mark_job_succeeded(&pool, job.id).await?;
        return Ok(JobOutcome::Succeeded);
    }
    if text.trim().is_empty() {
        mark_job_succeeded(&pool, job.id).await?;
        return Ok(JobOutcome::Succeeded);
    }

    let Some(ai) = ai_providers else {
        // No provider service wired — cannot embed; leave the item `pending`.
        mark_job_succeeded(&pool, job.id).await?;
        return Ok(JobOutcome::Succeeded);
    };

    // The kernel calls the embedding provider directly (no plugin principal —
    // kernel-initiated embedding is always authorized). `AiProviderService::embed`
    // applies the P11c per-op timeout + circuit breaker internally.
    let caller = crate::services::ai_provider::AiCaller::kernel(KERNEL_EMBED_PLUGIN, true);
    match ai.embed(&text, &caller).await {
        Ok(Some(result)) => {
            match vector_store {
                Some(store) if store.is_available().await => {
                    if let Err(e) = store
                        .store_embedding(
                            item.id,
                            crate::content::item_service::KERNEL_INDEX_FIELD,
                            &result.model,
                            &result.vector,
                        )
                        .await
                    {
                        // Storage failure (DB/pgvector) is transient — retry/DLQ.
                        let err = format!("store embedding: {e}");
                        let outcome =
                            mark_job_failed(&pool, &job, KERNEL_EMBED_PLUGIN, &err).await?;
                        if matches!(outcome, JobOutcome::DeadLettered) {
                            let _ = embed_index::mark_failed(&pool, item.id, &err).await;
                        }
                        return Ok(outcome);
                    }
                    let _ = embed_index::mark_indexed(&pool, item.id, &result.model, &current_hash)
                        .await;
                    mark_job_succeeded(&pool, job.id).await?;
                    Ok(JobOutcome::Succeeded)
                }
                _ => {
                    // Got a vector but no pgvector backend to store it — a
                    // graceful skip, not a failure. Leave `pending`; backfill
                    // re-embeds once a backend exists.
                    debug!(item_id = %item.id, "embedded but no vector backend; leaving pending");
                    mark_job_succeeded(&pool, job.id).await?;
                    Ok(JobOutcome::Succeeded)
                }
            }
        }
        // No embedding provider resolved — leave `pending`, drop the job (a
        // config gap, not a transient failure; retrying would not help).
        Ok(None) => {
            mark_job_succeeded(&pool, job.id).await?;
            Ok(JobOutcome::Succeeded)
        }
        // Provider failure (transport/HTTP-status/parse) — the retry→DLQ path
        // that replaces PF-4 sub-decision 3's silent drop (D-51).
        Err(e) => {
            let err = format!("embed provider failed: {e}");
            let outcome = mark_job_failed(&pool, &job, KERNEL_EMBED_PLUGIN, &err).await?;
            if matches!(outcome, JobOutcome::DeadLettered) {
                let _ = embed_index::mark_failed(&pool, item.id, &err).await;
            }
            Ok(outcome)
        }
    }
}

/// Redis key that holds the once-a-day claim on pruning search analytics.
const SEARCH_ANALYTICS_PRUNE_KEY: &str = "cron:search_analytics_prune";

/// Cron service for scheduled operations.
pub struct CronService {
    redis: RedisClient,
    pool: PgPool,
    tasks: CronTasks,
    queue: Arc<RedisQueue>,
    tap_dispatcher: Option<Arc<TapDispatcher>>,
    ai_providers: Option<Arc<AiProviderService>>,
    ai_budgets: Option<Arc<AiTokenBudgetService>>,
    /// pgvector store the native embed drain writes into (P11f). `None` in
    /// builds without embedding wired; the embed drain is a no-op then.
    vector_store: Option<Arc<PgVectorStore>>,
    http: reqwest::Client,
    pagefind_enabled: bool,
    /// The item-access seam the Pagefind export is filtered through, and the
    /// item service the background dispatch services carry for `item-api`.
    /// `None` in a harness that builds a `CronService` on its own, where the
    /// export is skipped rather than written unfiltered — the index is a
    /// world-readable file, so "no access model available" has to mean "publish
    /// nothing".
    items: Option<Arc<ItemService>>,
    /// The gather service the background dispatch services carry for the
    /// `query` host interface. `None` only in a harness that builds a
    /// `CronService` on its own; a `query` call from `tap_cron` or
    /// `tap_queue_worker` then gets `ERR_NO_SERVICES`.
    gather: Option<Arc<GatherService>>,
    /// The directory the generated Pagefind index is written to.
    ///
    /// Defaults to `./static/pagefind` so a harness without a `Config` still has
    /// a destination; `apply_runtime_config` sets it from `PAGEFIND_OUTPUT_DIR`.
    pagefind_output_dir: PathBuf,
    /// The site's rate limiter, threaded into background dispatch so the
    /// per-plugin `mail` bucket applies from `tap_cron` and `tap_queue_worker`
    /// and not only from a request.
    rate_limiter: Option<Arc<crate::middleware::RateLimiter>>,
    /// The site's one email service, the same `Arc` its own `email:send` queue
    /// uses, threaded into background dispatch so a plugin's `mail` calls from
    /// `tap_cron` and `tap_queue_worker` send rather than report that SMTP is
    /// not configured.
    email: Option<Arc<crate::services::email::EmailService>>,
    /// Wall-clock budget for one drain pass ([`QUEUE_DRAIN_BUDGET_SECS`]).
    /// Carried here rather than read from the constant so a deployment can tune
    /// it and a test can drive it to a value it can observe.
    drain_budget: Duration,
    /// How far past a worker's CPU budget the drain waits on an in-flight job
    /// before abandoning it (`JOB_ABANDON_SLACK`).
    abandon_slack: Duration,
    /// An outright override of the in-flight ceiling, ignoring the worker's
    /// budget. `None` in production, where the ceiling is budget + slack.
    job_ceiling: Option<Duration>,
    /// Bound on each ordinary cron task ([`CRON_TASK_TIMEOUT_SECS`]). A field
    /// rather than the constant so a test can drive it to a value it can wait
    /// out.
    task_timeout: Duration,
    /// Search, whose analytics tables cron prunes once a day. `None` in a
    /// harness that builds a `CronService` on its own.
    search: Option<Arc<crate::search::SearchService>>,
    /// Process-level override of the built-in scheduler's interval
    /// (`CRON_INTERVAL_SECS`), set by `apply_runtime_config`. `None`
    /// in a harness that never calls it, where `site_config` alone decides.
    scheduler_interval_override: Option<u64>,
    /// Runs this process's built-in scheduler has completed. Other callers'
    /// runs are not counted, which is what lets a test tell the scheduler's
    /// runs apart from cron that other processes run on the same site.
    scheduled_runs_completed: AtomicU64,
}

impl CronService {
    /// Create a new cron service.
    pub fn new(redis: RedisClient, pool: PgPool) -> Self {
        let queue = Arc::new(RedisQueue::new(redis.clone()));
        let tasks = CronTasks::new(pool.clone(), queue.clone());
        Self {
            redis,
            pool,
            tasks,
            queue,
            tap_dispatcher: None,
            ai_providers: None,
            ai_budgets: None,
            vector_store: None,
            http: build_http_client(),
            items: None,
            gather: None,
            pagefind_enabled: false,
            pagefind_output_dir: PathBuf::from("./static/pagefind"),
            rate_limiter: None,
            email: None,
            drain_budget: Duration::from_secs(QUEUE_DRAIN_BUDGET_SECS),
            abandon_slack: JOB_ABANDON_SLACK,
            job_ceiling: None,
            task_timeout: Duration::from_secs(CRON_TASK_TIMEOUT_SECS),
            search: None,
            scheduler_interval_override: None,
            scheduled_runs_completed: AtomicU64::new(0),
        }
    }

    /// Create a new cron service with file service for proper cleanup.
    pub fn with_file_service(redis: RedisClient, pool: PgPool, files: Arc<FileService>) -> Self {
        let queue = Arc::new(RedisQueue::new(redis.clone()));
        let tasks = CronTasks::with_file_service(pool.clone(), queue.clone(), files);
        Self {
            redis,
            pool,
            tasks,
            queue,
            tap_dispatcher: None,
            ai_providers: None,
            ai_budgets: None,
            vector_store: None,
            http: build_http_client(),
            items: None,
            gather: None,
            pagefind_enabled: false,
            pagefind_output_dir: PathBuf::from("./static/pagefind"),
            rate_limiter: None,
            email: None,
            drain_budget: Duration::from_secs(QUEUE_DRAIN_BUDGET_SECS),
            abandon_slack: JOB_ABANDON_SLACK,
            job_ceiling: None,
            task_timeout: Duration::from_secs(CRON_TASK_TIMEOUT_SECS),
            search: None,
            scheduler_interval_override: None,
            scheduled_runs_completed: AtomicU64::new(0),
        }
    }

    /// Attach the site's rate limiter, arming the per-plugin `mail` bucket on
    /// the cron and queue-worker paths.
    ///
    /// Without it a plugin calling `mail` from a background tap is unbounded,
    /// which is the gap this closes.
    /// Override the wall-clock budget for one drain pass.
    ///
    /// The default is `QUEUE_DRAIN_BUDGET_SECS`. Lowering it makes each pass
    /// give the cron lock back sooner at the cost of more passes.
    pub fn set_drain_budget(&mut self, budget: Duration) {
        self.drain_budget = budget;
    }

    /// Override how long the drain waits past a worker's CPU budget before it
    /// abandons an in-flight job.
    ///
    /// The default is `JOB_ABANDON_SLACK`. Tests lower it so a job that will
    /// never come back is abandoned in seconds rather than minutes.
    pub fn set_abandon_slack(&mut self, slack: Duration) {
        self.abandon_slack = slack;
    }

    /// Override the in-flight ceiling outright, ignoring the worker's budget.
    ///
    /// Only tests use this. A test needs to abandon a job *before* the epoch
    /// deadline would reclaim it, which a ceiling derived from that same
    /// deadline can never do.
    pub fn set_job_ceiling(&mut self, ceiling: Duration) {
        self.job_ceiling = Some(ceiling);
    }

    /// Override the bound on each ordinary cron task.
    ///
    /// The default is `CRON_TASK_TIMEOUT_SECS`. Tests lower it so a task stuck
    /// on a Redis that never answers is cut off in a second, not minutes.
    pub fn set_task_timeout(&mut self, timeout: Duration) {
        self.task_timeout = timeout;
    }

    /// Point the cron tasks' Redis queue (`email:send`, `search:reindex`) at
    /// `redis`, leaving the lock and everything else on the client the service
    /// was built with.
    ///
    /// Only tests use this: it is how a run is given a queue that never answers
    /// while its lock still works.
    pub fn set_queue_redis(&mut self, redis: RedisClient) {
        let queue = Arc::new(RedisQueue::new(redis));
        self.tasks.set_queue(queue.clone());
        self.queue = queue;
    }

    /// The outer bound on one queue drain inside a cron run.
    ///
    /// A drain already stops starting batches after `drain_budget`, and waits on
    /// the batch in flight for at most the in-flight ceiling (a worker's epoch
    /// budget plus `abandon_slack`, or the test override). What it did not bound
    /// is everything around those: the claims, the reaping, the bookkeeping, all
    /// of them Redis or database calls. This is the sum of the two bounds the
    /// drain enforces itself, plus one ordinary task's allowance for the rest,
    /// so it only fires when the drain is stuck somewhere its own bounds do not
    /// reach. Work cut off here is not lost: its rows stay claimed until their
    /// lease runs out and are claimed again on a later cycle.
    fn drain_timeout(&self) -> Duration {
        let worker_budget = self
            .tap_dispatcher
            .as_ref()
            .map(|d| d.runtime().limits().background_tap_epoch_deadline_secs)
            .unwrap_or(0);
        let in_flight = self
            .job_ceiling
            .unwrap_or(Duration::from_secs(worker_budget) + self.abandon_slack);
        self.drain_budget + in_flight + self.task_timeout
    }

    pub fn set_rate_limiter(&mut self, limiter: Arc<crate::middleware::RateLimiter>) {
        self.rate_limiter = Some(limiter);
    }

    /// Configure the update check, or disable it with `None`.
    ///
    /// `apply_runtime_config` sets this from the process configuration. It is public
    /// so a test can point the check at a local endpoint and drive the same code
    /// the cron cycle drives, rather than a mock of it.
    pub fn set_update_check(&mut self, update_check: Option<UpdateCheckConfig>) {
        self.tasks.set_update_check(update_check);
    }

    /// Run the update check now, subject to the setting and the interval.
    ///
    /// # Errors
    ///
    /// Returns the request or storage error. The cron cycle logs it at debug and
    /// carries on.
    pub async fn check_for_updates(&self) -> Result<Option<crate::update_status::UpdateStatus>> {
        self.tasks.check_for_updates().await
    }

    /// Set the tap dispatcher for plugin cron hooks.
    pub fn set_tap_dispatcher(&mut self, dispatcher: Arc<TapDispatcher>) {
        self.tap_dispatcher = Some(dispatcher);
    }

    /// Set the AI provider service for cron plugin access.
    pub fn set_ai_providers(&mut self, ai_providers: Arc<AiProviderService>) {
        self.ai_providers = Some(ai_providers);
    }

    /// Set the AI token budget service for cron plugin access.
    pub fn set_ai_budgets(&mut self, ai_budgets: Arc<AiTokenBudgetService>) {
        self.ai_budgets = Some(ai_budgets);
    }

    /// Set the pgvector store for the native embed drain (P11f).
    pub fn set_vector_store(&mut self, vector_store: Arc<PgVectorStore>) {
        self.vector_store = Some(vector_store);
    }

    /// Set the item-access seam the Pagefind export is filtered through, and
    /// the item service `tap_cron` and `tap_queue_worker` reach `item-api` with.
    ///
    /// Wired from `AppState`, which already owns the one `ItemService` the rest
    /// of the site uses: a second one built here would carry its own
    /// field-access cache and a different tap dispatcher.
    pub fn set_item_service(&mut self, items: Arc<ItemService>) {
        self.items = Some(items);
    }

    /// Set the gather service `tap_cron` and `tap_queue_worker` reach the
    /// `query` host interface with.
    ///
    /// Wired from `AppState`, which owns the one `GatherService` request
    /// dispatch already binds, so a background read runs the same seam.
    pub fn set_gather_service(&mut self, gather: Arc<GatherService>) {
        self.gather = Some(gather);
    }

    /// Wire in search, so cron prunes its analytics past the retention window.
    pub fn set_search_service(&mut self, search: Arc<crate::search::SearchService>) {
        self.search = Some(search);
    }

    /// Enable pagefind index rebuilding (requires `trovato_search` plugin).
    pub fn set_pagefind_enabled(&mut self, enabled: bool) {
        self.pagefind_enabled = enabled;
    }

    /// Apply the configuration the cron tasks used to read from the environment.
    ///
    /// The Pagefind index needs one destination: `PAGEFIND_OUTPUT_DIR`, which
    /// defaults to `pagefind` under the **base** (first) static directory.
    /// Writing it into every root would leave several copies of a generated
    /// artifact to keep in step, and writing it into the last root would mean a
    /// cron job dropping build output into an application's own repository.
    pub fn apply_runtime_config(&mut self, runtime: &crate::config::RuntimeConfig) {
        self.tasks
            .set_security_audit_retention_days(runtime.security_audit_retention_days);
        self.tasks.set_system_log_retention(
            runtime.system_log_retention_days,
            runtime.system_log_max_rows,
        );
        // The update check is off unless the process allows it. The client is the
        // same SSRF-hardened outbound client the plugin fetchers use: this request
        // goes to a name resolved at runtime, so it gets the same rebinding-safe
        // resolver and per-hop redirect revalidation as any other.
        self.tasks
            .set_update_check(runtime.update_check_enabled.then(|| {
                crate::cron::tasks::UpdateCheckConfig {
                    endpoint: runtime.update_check_endpoint.clone(),
                    client: build_http_client(),
                    interval_secs: runtime.update_check_interval_secs,
                }
            }));
        self.pagefind_output_dir = runtime.pagefind_output_dir.clone();
        self.scheduler_interval_override = runtime.cron_interval_override;
    }

    /// Set optional plugin services for cron tasks.
    pub fn set_plugin_services(
        &mut self,
        content_lock: Option<std::sync::Arc<crate::services::content_lock::ContentLockService>>,
        audit: Option<std::sync::Arc<crate::services::audit::AuditService>>,
    ) {
        self.tasks.set_plugin_services(content_lock, audit);
    }

    /// Set the email service: for sending queued emails, and for the plugin
    /// `mail` calls made from `tap_cron` and `tap_queue_worker`.
    pub fn set_email_service(
        &mut self,
        email: Option<std::sync::Arc<crate::services::email::EmailService>>,
    ) {
        self.email = email.clone();
        self.tasks.set_email_service(email);
    }

    /// Run all cron tasks.
    ///
    /// Acquires a distributed lock before running to ensure only one
    /// instance executes cron at a time.
    pub async fn run(&self) -> CronResult {
        self.run_with(None).await
    }

    /// Run all cron tasks unless a run finished less than `interval_secs` ago
    /// (B178): the built-in scheduler's entry point.
    ///
    /// The scheduler decides a run is due before it calls this, but two
    /// servers can both read the same old `cron:last_run`, both decide, and
    /// then take the lock one after the other, the second one running cron
    /// again right after the first. So the check is repeated here, after the
    /// lock is held, where no other run can finish in between: a run that is
    /// no longer due gives the lock back and is reported as skipped.
    pub async fn run_if_due(&self, interval_secs: u64) -> CronResult {
        self.run_with(Some(interval_secs)).await
    }

    async fn run_with(&self, due_after_secs: Option<u64>) -> CronResult {
        let start = std::time::Instant::now();

        // Try to acquire lock
        let lock_value = match self.acquire_lock().await {
            Ok(Some(v)) => v,
            Ok(None) => {
                self.report_held_lock().await;
                return CronResult::Skipped;
            }
            Err(e) => {
                warn!(error = %e, "failed to acquire cron lock");
                return CronResult::Failed(e.to_string());
            }
        };

        if let Some(interval_secs) = due_after_secs {
            let last_finished = self.last_run().await.ok().flatten().map(|r| r.timestamp);
            if !scheduled_run_due(chrono::Utc::now().timestamp(), last_finished, interval_secs) {
                debug!("cron ran less than an interval ago; giving the lock back");
                if let Err(e) = self.release_lock(&lock_value).await {
                    warn!(error = %e, "failed to release cron lock");
                }
                return CronResult::Skipped;
            }
        }

        info!("acquired cron lock, running tasks");

        // Start heartbeat task
        let (stop_tx, stop_rx) = watch::channel(false);
        let heartbeat_redis = self.redis.clone();
        let heartbeat_lock = lock_value.clone();
        let heartbeat_handle = tokio::spawn(async move {
            run_heartbeat(
                heartbeat_redis,
                &heartbeat_lock,
                stop_rx,
                HeartbeatConfig::default(),
            )
            .await;
        });

        // Run tasks
        let mut tasks_run = Vec::new();

        // Cleanup temporary files
        match bounded(
            "cleanup_temp_files",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_temp_files(),
        )
        .await
        {
            Some(Ok(count)) => {
                info!(deleted = count, "cleaned up temporary files");
                tasks_run.push(format!("cleanup_temp_files: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to cleanup temp files"),
            None => {}
        }

        // Cleanup expired sessions
        match bounded(
            "cleanup_expired_sessions",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_expired_sessions(),
        )
        .await
        {
            Some(Ok(count)) => {
                info!(deleted = count, "cleaned up expired sessions");
                tasks_run.push(format!("cleanup_expired_sessions: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to cleanup sessions"),
            None => {}
        }

        // Cleanup form state cache
        match bounded(
            "cleanup_form_state_cache",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_form_state_cache(),
        )
        .await
        {
            Some(Ok(count)) => {
                info!(deleted = count, "cleaned up form state cache");
                tasks_run.push(format!("cleanup_form_state_cache: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to cleanup form state"),
            None => {}
        }

        // Process queues
        match bounded(
            "process_queues",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.process_queues(),
        )
        .await
        {
            Some(Ok(count)) => {
                info!(processed = count, "processed queue items");
                tasks_run.push(format!("process_queues: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to process queues"),
            None => {}
        }

        // Ask whether a newer release exists. Failures are logged at debug and
        // change nothing: an unreachable GitHub is not a site problem, and this is
        // the one task whose output is advisory.
        match bounded(
            "check_for_updates",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.check_for_updates(),
        )
        .await
        {
            Some(Ok(Some(status))) => {
                info!(
                    latest = %status.latest_version,
                    is_security = status.is_security,
                    "checked for updates"
                );
                tasks_run.push(format!("check_for_updates: {}", status.latest_version));
            }
            Some(Ok(None)) => {}
            Some(Err(e)) => debug!(error = %e, "update check failed; nothing changed"),
            None => {}
        }

        // Cleanup expired verification tokens
        match bounded(
            "cleanup_verification_tokens",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_verification_tokens(),
        )
        .await
        {
            Some(Ok(count)) if count > 0 => {
                info!(count = count, "cleaned up expired verification tokens");
                tasks_run.push(format!("cleanup_verification_tokens: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to cleanup verification tokens"),
            _ => {}
        }

        // Cleanup expired password reset tokens
        match bounded(
            "cleanup_password_reset_tokens",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_password_reset_tokens(),
        )
        .await
        {
            Some(Ok(count)) if count > 0 => {
                info!(count = count, "cleaned up expired password reset tokens");
                tasks_run.push(format!("cleanup_password_reset_tokens: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to cleanup password reset tokens"),
            _ => {}
        }

        // Cleanup expired content locks
        match bounded(
            "cleanup_expired_locks",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_expired_locks(),
        )
        .await
        {
            Some(Ok(count)) if count > 0 => {
                info!(count = count, "cleaned up expired locks");
                tasks_run.push(format!("cleanup_expired_locks: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to cleanup locks"),
            _ => {}
        }

        // Mail retention (BG18)
        match bounded(
            "purge_mail_outbox",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.purge_mail_outbox(),
        )
        .await
        {
            Some(Ok((cleared, deleted))) if cleared + deleted > 0 => {
                info!(
                    cleared,
                    deleted, "mail retention: bodies cleared, old messages deleted"
                );
                tasks_run.push(format!(
                    "purge_mail_outbox: {cleared} cleared, {deleted} deleted"
                ));
            }
            Some(Err(e)) => warn!(error = %e, "failed to apply mail retention"),
            _ => {}
        }

        // Cleanup audit log (periodic)
        match bounded(
            "cleanup_audit_log",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_audit_log(),
        )
        .await
        {
            Some(Ok(count)) if count > 0 => {
                info!(count = count, "cleaned up old audit log entries");
                tasks_run.push(format!("cleanup_audit_log: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to cleanup audit log"),
            _ => {}
        }

        // Keep the site log to its caps (`/admin/reports/log`).
        match bounded(
            "cleanup_system_log",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_system_log(),
        )
        .await
        {
            Some(Ok(count)) if count > 0 => {
                info!(count = count, "pruned the system log");
                tasks_run.push(format!("cleanup_system_log: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to prune the system log"),
            _ => {}
        }

        // Prune the kernel-internal security audit stream (one bounded policy)
        match bounded(
            "cleanup_security_audit_log",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.cleanup_security_audit_log(),
        )
        .await
        {
            Some(Ok(count)) if count > 0 => {
                info!(count = count, "pruned old security audit events");
                tasks_run.push(format!("cleanup_security_audit_log: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to prune security audit log"),
            _ => {}
        }

        // Catch up items an earlier release never scanned for images without
        // a text alternative (B251), one bounded batch per run.
        match bounded(
            "backfill_accessibility_scan",
            self.task_timeout,
            &mut tasks_run,
            self.tasks.backfill_accessibility_scan(),
        )
        .await
        {
            Some(Ok(count)) if count > 0 => {
                info!(
                    count = count,
                    "scanned content for images without a text alternative"
                );
                tasks_run.push(format!("backfill_accessibility_scan: {count}"));
            }
            Some(Err(e)) => warn!(error = %e, "failed to backfill the accessibility scan"),
            _ => {}
        }

        // Dispatch tap_cron to all plugins that implement it
        if let Some(ref dispatcher) = self.tap_dispatcher {
            let expected = dispatcher.registry().handler_count("tap_cron");
            if expected > 0 {
                let cron_input = trovato_sdk::types::CronInput {
                    timestamp: chrono::Utc::now().timestamp(),
                };
                // Infallible: CronInput is a flat struct with a single i64 field.
                let input_json = serde_json::to_string(&cron_input)
                    .unwrap_or_else(|_| r#"{"timestamp":0}"#.to_string());
                // P11c / D-40: cron dispatch carries the kernel-internal
                // background principal so a plugin holding the `ai_background`
                // capability may call AI from `tap_cron`. The marker never
                // reaches a web/session context.
                let state = RequestState::new(
                    crate::tap::UserContext::background(),
                    self.background_services(dispatcher),
                );
                match tokio::time::timeout(
                    Duration::from_secs(LOCK_TTL_SECS / 2),
                    dispatcher.dispatch("tap_cron", &input_json, state),
                )
                .await
                {
                    Ok(results) => {
                        let failed = expected - results.len();
                        for result in &results {
                            info!(plugin = %result.plugin_name, "tap_cron completed");
                            tasks_run.push(format!("tap_cron:{}", result.plugin_name));
                        }
                        if failed > 0 {
                            warn!(
                                expected = expected,
                                succeeded = results.len(),
                                failed = failed,
                                "some tap_cron handlers failed (see dispatcher errors above)"
                            );
                        }
                    }
                    Err(_) => {
                        warn!(
                            expected = expected,
                            timeout_secs = LOCK_TTL_SECS / 2,
                            "tap_cron dispatch timed out"
                        );
                        tasks_run.push("tap_cron:TIMEOUT".to_string());
                    }
                }
            }
        }

        // Drain the plugin queue with v2 semantics (P11d / D-45..D-47): claim
        // locking, backoff, retry accounting, and dead-lettering.
        //
        // After tap_cron runs, plugins may have pushed jobs via queue_push /
        // enqueue. When the opt-in resident queue-runner is enabled (D-46), it
        // owns draining and the in-request cron drain steps aside to avoid
        // double-draining; otherwise this cron run, whoever triggered it, drives
        // the drain (the default cadence contract).
        //
        // The kernel-internal embed queue (P11f) is drained on the same cadence
        // and follows the same resident-runner hand-off, but is independent of
        // any plugin `tap_queue_worker`, so it runs even when no plugin
        // implements one.
        // A config read that cannot finish counts as "runner off", the answer a
        // failed read already gives: the cron drain then runs, and the `SKIP
        // LOCKED` claims make that safe even if a runner is draining too.
        let runner_owns_drain = bounded(
            "queue_runner_config",
            self.task_timeout,
            &mut tasks_run,
            self.queue_runner_config(),
        )
        .await
        .is_some_and(|config| config.enabled);
        if runner_owns_drain {
            debug!("resident queue-runner enabled; cron skips queue drains (D-46)");
        } else {
            if let Some(ref dispatcher) = self.tap_dispatcher
                && dispatcher.registry().has_tap("tap_queue_worker")
            {
                match bounded(
                    "tap_queue_worker",
                    self.drain_timeout(),
                    &mut tasks_run,
                    self.drain_plugin_queues(),
                )
                .await
                {
                    Some(Ok(stats)) => {
                        tasks_run.push(format!("tap_queue_worker: {}", stats.total()));
                    }
                    Some(Err(e)) => warn!(error = %e, "plugin queue dispatch failed"),
                    None => {}
                }
            }
            match bounded(
                "embed_jobs",
                self.drain_timeout(),
                &mut tasks_run,
                self.drain_embed_jobs(),
            )
            .await
            {
                Some(Ok(stats)) => {
                    if stats.total() > 0 || stats.errors > 0 {
                        tasks_run.push(format!("embed_jobs: {}", stats.total()));
                    }
                }
                Some(Err(e)) => warn!(error = %e, "embed queue drain failed"),
                None => {}
            }
        }

        // Prune search analytics past the retention window, at most once a day.
        // One DELETE, so cutting it off at the task bound loses nothing: the
        // rows it did not reach are pruned on a later day.
        if let Some(ref search) = self.search {
            let prune = async {
                match self.claim_daily(SEARCH_ANALYTICS_PRUNE_KEY).await {
                    Ok(true) => {
                        let retention = search.settings().await.analytics_retention_days;
                        match search.analytics().prune(retention).await {
                            Ok(count) => Some(format!("prune_search_analytics: {count}")),
                            Err(e) => {
                                warn!(error = %e, "failed to prune search analytics");
                                None
                            }
                        }
                    }
                    Ok(false) => None,
                    Err(e) => {
                        warn!(error = %e, "failed to claim the daily search analytics prune");
                        None
                    }
                }
            };
            if let Some(Some(entry)) = bounded(
                "prune_search_analytics",
                self.task_timeout,
                &mut tasks_run,
                prune,
            )
            .await
            {
                tasks_run.push(entry);
            }
        }

        // Rebuild Pagefind index if the trovato_search plugin is enabled and requested it
        if self.pagefind_enabled {
            if let Some(items) = self.items.clone() {
                // A rebuild is never cut off part way. A requested full build
                // clears its request before it starts and sets it again only
                // when it ends without building, so a cancelled one would lose
                // the request; and the work done so far would be thrown away.
                // It runs on a task of its own instead, and the run waits on it
                // for one task's bound, then moves on and leaves it to finish.
                // Publishing is already safe against any other writer, in this
                // process or another: every publish holds the `search_index`
                // advisory lock and checks the index generation
                // (`search_update`). The guard only spares this process a
                // second, redundant build while the first is still going.
                match PagefindRebuild::start() {
                    None => {
                        debug!("a pagefind rebuild is still running; not starting another");
                    }
                    Some(guard) => {
                        let pool = self.pool.clone();
                        let output_dir = self.pagefind_output_dir.clone();
                        let search = self.search.clone();
                        let rebuild = tokio::spawn(async move {
                            let _guard = guard;
                            let compaction = match search {
                                Some(ref search) => {
                                    search.settings().await.index_compaction_percent
                                }
                                None => DEFAULT_COMPACTION_PERCENT,
                            };
                            pagefind::maybe_rebuild_index(&pool, &output_dir, &items, compaction)
                                .await
                        });
                        match bounded(
                            "pagefind_rebuild",
                            self.task_timeout,
                            &mut tasks_run,
                            rebuild,
                        )
                        .await
                        {
                            Some(Ok(Ok(true))) => tasks_run.push("pagefind_rebuild".to_string()),
                            Some(Ok(Ok(false))) => {}
                            Some(Ok(Err(e))) => warn!(error = %e, "pagefind index rebuild failed"),
                            Some(Err(e)) => warn!(error = %e, "pagefind index rebuild panicked"),
                            None => {}
                        }
                    }
                }
            } else {
                warn!(
                    "pagefind rebuild skipped: no item service is wired into this cron \
                     service, and the index is a world-readable file that may not be \
                     written without an access decision"
                );
            }
        }

        // Stop heartbeat
        let _ = stop_tx.send(true);
        let _ = heartbeat_handle.await;

        // Release lock
        if let Err(e) = self.release_lock(&lock_value).await {
            warn!(error = %e, "failed to release cron lock");
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(duration_ms = duration_ms, tasks = ?tasks_run, "cron completed");

        let result = CronResult::Completed {
            tasks_run,
            duration_ms,
        };

        if let Err(e) = self.record_run(&result).await {
            warn!(error = %e, "failed to record cron run");
        }

        result
    }

    /// Acquire the distributed cron lock.
    ///
    /// Returns the lock value if acquired, None if already held.
    async fn acquire_lock(&self) -> Result<Option<String>> {
        // Unique per run, not per process. The value used to be `host:pid`, so
        // two runs in one process (the built-in scheduler and `/cron/{key}`,
        // say) wrote the same value, and the owner check in `release_lock`
        // could not tell them apart: a run that outlived its lock (renewal
        // stops after `MAX_LOCK_RENEWAL_SECS`) would, on finishing, delete the
        // lock a newer run in the same process had since taken, letting a
        // third caller start alongside it. The host and pid stay as a prefix
        // for whoever reads the key by hand.
        let lock_value = new_lock_value();

        let mut conn = self
            .redis
            .bounded_connection()
            .await
            .context("failed to get Redis connection")?;

        // SET NX EX - set only if not exists, with expiry
        let result: Option<String> = redis::cmd("SET")
            .arg(CRON_LOCK_KEY)
            .arg(&lock_value)
            .arg("NX")
            .arg("EX")
            .arg(LOCK_TTL_SECS)
            .query_async(&mut conn)
            .await
            .context("failed to acquire lock")?;

        Ok(result.map(|_| lock_value))
    }

    /// Say why this run is skipping: at debug for an ordinary overlap, at warn
    /// when the holder has had the lock longer than any run should.
    ///
    /// This was a bare `debug!` (B247). In the incident behind this, a stuck
    /// run held the lock for ten hours and every tick in between logged only
    /// that line, which production does not show, so cron looked idle rather
    /// than wedged. Reading the lock's age turns the second case into a line an
    /// operator sees.
    async fn report_held_lock(&self) {
        let read = async {
            let mut conn = self.redis.bounded_connection().await?;
            redis::pipe()
                .cmd("GET")
                .arg(CRON_LOCK_KEY)
                .cmd("TTL")
                .arg(CRON_LOCK_KEY)
                .query_async::<(Option<String>, i64)>(&mut conn)
                .await
        };
        let (holder, ttl_secs) = match read.await {
            Ok((Some(holder), ttl)) => (holder, ttl),
            Ok((None, _)) => {
                debug!("cron lock was held a moment ago and is free now; skipping this run");
                return;
            }
            Err(e) => {
                debug!(error = %e, "cron lock held by another instance, skipping; could not read its age");
                return;
            }
        };
        match classify_held_lock(&holder, ttl_secs, chrono::Utc::now().timestamp()) {
            HeldLock::Overlap { held_secs } => debug!(
                holder = %holder,
                held_secs = ?held_secs,
                ttl_secs,
                "cron lock held by another instance, skipping"
            ),
            HeldLock::TooLong { held_secs } => warn!(
                holder = %holder,
                held_secs,
                ttl_secs,
                max_secs = MAX_LOCK_RENEWAL_SECS,
                "cron lock has been held for {held_secs}s, longer than any cron run should \
                 take; skipping this run. The holder is probably stuck: the lock expires on \
                 its own in {ttl_secs}s"
            ),
            HeldLock::NoExpiry => warn!(
                holder = %holder,
                held_secs = ?lock_held_since(&holder)
                    .map(|since| chrono::Utc::now().timestamp() - since),
                "cron lock has no expiry, so no cron run will ever take it again until it is \
                 deleted; skipping this run"
            ),
        }
    }

    /// Release the distributed cron lock.
    ///
    /// Only releases if we own the lock (checked via Lua script).
    async fn release_lock(&self, lock_value: &str) -> Result<()> {
        let mut conn = self
            .redis
            .bounded_connection()
            .await
            .context("failed to get Redis connection")?;

        // Use Lua script to atomically check and delete
        let script = redis::Script::new(RELEASE_LOCK_SCRIPT);
        script
            .key(CRON_LOCK_KEY)
            .arg(lock_value)
            .invoke_async::<()>(&mut conn)
            .await
            .context("failed to release lock")?;

        debug!("released cron lock");
        Ok(())
    }

    /// Get the queue for pushing items.
    pub fn queue(&self) -> &Arc<RedisQueue> {
        &self.queue
    }

    /// Drain the plugin queue with v2 semantics (P11d / D-45..D-47).
    ///
    /// For each plugin with claimable items, and then for **each of that
    /// plugin's queues**, claims batches of up to that queue's honored
    /// concurrency (D-47: parsed from `tap_queue_info`, clamped to
    /// `QUEUE_CONCURRENCY_CAP`) via `FOR UPDATE SKIP LOCKED`, dispatches
    /// `tap_queue_worker` on each claimed item **in parallel**, and records the
    /// terminal outcome:
    ///
    /// - success (`tap_queue_worker` returns): the item is deleted;
    /// - failure (worker traps or returns an error result) with
    ///   `attempts < max_attempts`: the item is rescheduled with exponential
    ///   backoff (`next_attempt_at = now + backoff`);
    /// - failure at `attempts >= max_attempts`: the item is dead-lettered
    ///   (`status = 'dead'`), so a poison item never blocks its queue or retries
    ///   forever.
    ///
    /// Delivery is **at-least-once** (D-47): a job whose claimer crashes mid-work
    /// keeps its incremented `attempts` and becomes reclaimable once its lease
    /// (`locked_until`) expires. Queue workers must therefore be idempotent —
    /// this is precisely the duplicate-row class the reference importer hit
    /// (`ritrovo_importer/migrations/002_dedup_conferences.sql`).
    ///
    /// A per-plugin per-cycle cap (`MAX_QUEUE_ITEMS_PER_CYCLE`) preserves
    /// fairness: one plugin flooding its queue cannot starve another.
    ///
    /// A pass is also bounded in *time* by `QUEUE_DRAIN_BUDGET_SECS`, checked
    /// between batches. The item cap alone did not bound it: a job may occupy a
    /// worker for the whole background epoch deadline, so a cap of 100 items at
    /// width 4 allowed a pass lasting over an hour, all of it inside a cron run
    /// holding the global lock. Work left over when the budget runs out is
    /// claimable again on the next cycle.
    ///
    /// Called both from the in-request cron drain ([`Self::run`]) and, when
    /// enabled, from the resident queue-runner ([`Self::run_queue_runner`]); the
    /// `SKIP LOCKED` claims make concurrent drainers safe.
    pub async fn drain_plugin_queues(&self) -> Result<QueueDrainStats> {
        let mut stats = QueueDrainStats::default();
        let deadline = std::time::Instant::now() + self.drain_budget;

        let Some(dispatcher) = self.tap_dispatcher.clone() else {
            return Ok(stats);
        };
        if !dispatcher.registry().has_tap("tap_queue_worker") {
            return Ok(stats);
        }

        // Plugins with claimable items: ready and past their backoff, or claimed
        // with an expired lease (a crashed claimer's work, reclaimed).
        let now = chrono::Utc::now().timestamp();
        let plugins: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT DISTINCT plugin_name
            FROM plugin_queue
            WHERE status IN ('ready', 'claimed')
              AND (
                    (attempts < max_attempts
                     AND (
                           (status = 'ready' AND next_attempt_at <= $1)
                        OR (status = 'claimed' AND locked_until <= $1)
                     ))
                 -- Exhausted-but-abandoned rows have no claimable work left, but
                 -- the plugin still has to be visited so the reaper can retire
                 -- them.
                 OR (status = 'claimed' AND locked_until <= $1
                     AND attempts >= max_attempts)
              )
            ORDER BY plugin_name
            "#,
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .context("failed to query plugin_queue")?;

        for plugin_name in &plugins {
            if std::time::Instant::now() >= deadline {
                debug!(
                    budget_secs = self.drain_budget.as_secs(),
                    "drain budget spent; remaining plugins wait for the next cycle"
                );
                break;
            }

            // Retire abandoned rows before claiming, so a plugin whose slots are
            // all held by exhausted rows gets them back in this pass.
            if let Err(e) = self.reap_exhausted_jobs(plugin_name).await {
                warn!(error = %e, plugin = %plugin_name, "failed to reap exhausted queue items");
            }

            // Skip plugins that don't implement tap_queue_worker.
            if !dispatcher
                .registry()
                .get_handlers("tap_queue_worker")
                .iter()
                .any(|h| &h.plugin.info.name == plugin_name)
            {
                continue;
            }

            // What the plugin declared, per queue, and which of its queues
            // actually hold work. A queue with rows but no declaration drains at
            // width 1 rather than being stranded.
            // The same budget a worker is dispatched with, so the drain's own
            // ceiling can be expressed relative to it.
            let budget = dispatcher
                .runtime()
                .limits()
                .background_tap_epoch_deadline_secs;
            let declared = self.plugin_queue_widths(&dispatcher, plugin_name).await;
            let queues = self.claimable_queues(plugin_name).await?;

            // The per-cycle cap stays PER PLUGIN: it is the fairness bound
            // between plugins (D-45), and giving each queue its own would let a
            // plugin multiply its share by declaring more queues.
            //
            // Its queues take turns rather than draining one at a time in name
            // order. A shared budget spent queue by queue would let the
            // alphabetically first queue consume the whole cycle and starve the
            // rest — cross-queue starvation introduced by the same change that
            // fixed cross-queue width, which is not a trade worth making. One
            // round gives every still-active queue exactly one batch of its own
            // width; a queue that comes back empty drops out of the rotation.
            let mut processed: i64 = 0;
            let mut active: Vec<&String> = queues.iter().collect();

            while processed < MAX_QUEUE_ITEMS_PER_CYCLE
                && !active.is_empty()
                && std::time::Instant::now() < deadline
            {
                let mut still_active = Vec::with_capacity(active.len());

                for queue_name in active {
                    if processed >= MAX_QUEUE_ITEMS_PER_CYCLE
                        || std::time::Instant::now() >= deadline
                    {
                        // Cap or time budget spent mid-round. Keep the queue in
                        // the rotation so the outer condition, not this queue,
                        // ends the cycle.
                        still_active.push(queue_name);
                        continue;
                    }
                    let width = declared.get(queue_name).copied().unwrap_or(1);
                    let batch = width.min((MAX_QUEUE_ITEMS_PER_CYCLE - processed) as usize);
                    let claimed = self.claim_batch(plugin_name, queue_name, batch).await?;
                    if claimed.is_empty() {
                        // Nothing claimable here any more; drop it from the
                        // rotation rather than re-querying it every round.
                        continue;
                    }
                    still_active.push(queue_name);
                    processed += claimed.len() as i64;

                    // Dispatch the claimed chunk in parallel — this is where a
                    // queue's declared concurrency actually executes
                    // concurrently. Each job owns its state and does its own
                    // delete/retry/dead-letter bookkeeping.
                    let mut set = tokio::task::JoinSet::new();
                    // What is still running, so a job the drain gives up on can
                    // be named and handed back to the queue rather than left
                    // claimed until its lock lapses.
                    let mut inflight: HashMap<tokio::task::Id, (ClaimedJob, Arc<AtomicU64>)> =
                        HashMap::new();
                    for job in claimed {
                        let pool = self.pool.clone();
                        let disp = dispatcher.clone();
                        let services = self.background_services(&dispatcher);
                        let plugin = plugin_name.clone();
                        let queue = queue_name.clone();
                        let sink = Arc::new(AtomicU64::new(0));
                        let job_record = job.clone();
                        let job_sink = sink.clone();
                        let handle = set.spawn(async move {
                            run_queue_job(pool, disp, services, plugin, queue, job, job_sink).await
                        });
                        inflight.insert(handle.id(), (job_record, sink));
                    }

                    // Wait for the batch, but not without end. A worker that
                    // stops executing stops answering to the epoch deadline too,
                    // and this `await` was the one place a single such job could
                    // hold the pass — and with it the cron lock — open.
                    let batch_started = std::time::Instant::now();
                    let batch_ceiling = tokio::time::Instant::now()
                        + self
                            .job_ceiling
                            .unwrap_or(Duration::from_secs(budget) + self.abandon_slack);
                    loop {
                        let joined =
                            tokio::time::timeout_at(batch_ceiling, set.join_next_with_id()).await;
                        match joined {
                            Ok(Some(Ok((id, Ok(outcome))))) => {
                                inflight.remove(&id);
                                stats.record(outcome);
                            }
                            Ok(Some(Ok((id, Err(e))))) => {
                                inflight.remove(&id);
                                warn!(error = %e, plugin = %plugin_name, queue = %queue_name, "queue job bookkeeping failed");
                                stats.errors += 1;
                            }
                            Ok(Some(Err(e))) => {
                                inflight.remove(&e.id());
                                warn!(error = %e, plugin = %plugin_name, queue = %queue_name, "queue job task panicked");
                                stats.errors += 1;
                            }
                            Ok(None) => break,
                            Err(_) => {
                                // Ceiling reached. Hand every job still running
                                // back to the queue, naming where it was stuck,
                                // then end the pass so the lock is released.
                                for (job, sink) in inflight.values() {
                                    let where_stuck = crate::host::trace::in_flight(
                                        sink.load(std::sync::atomic::Ordering::Relaxed),
                                    )
                                    .map_or_else(
                                        || "no host call in progress".to_string(),
                                        |c| c.qualified_name(),
                                    );
                                    error!(
                                        plugin = %plugin_name,
                                        queue = %queue_name,
                                        item_id = job.id,
                                        attempt = job.attempts,
                                        elapsed_ms = batch_started.elapsed().as_millis(),
                                        host_call = %where_stuck,
                                        outcome = "abandoned",
                                        "queue job abandoned: it outlasted the drain's ceiling and \
                                         was handed back to the queue"
                                    );
                                    let ceiling = self.job_ceiling.unwrap_or(
                                        Duration::from_secs(budget) + self.abandon_slack,
                                    );
                                    let err = format!(
                                        "abandoned after {}s in {where_stuck}",
                                        ceiling.as_secs()
                                    );
                                    match mark_job_failed(&self.pool, job, plugin_name, &err).await
                                    {
                                        Ok(outcome) => stats.record(outcome),
                                        Err(e) => {
                                            warn!(error = %e, plugin = %plugin_name, item_id = job.id, "failed to release an abandoned queue job");
                                            stats.errors += 1;
                                        }
                                    }
                                }
                                // Dropping the set aborts what it holds. An abort
                                // lands at the next await point, so a job waiting
                                // on I/O stops here; one burning CPU without
                                // yielding cannot be reclaimed in-process at all,
                                // and keeps its thread until the process restarts.
                                drop(set);
                                return Ok(stats);
                            }
                        }
                    }
                }

                active = still_active;
            }
        }

        Ok(stats)
    }

    /// Drain the kernel-internal embed queue (P11f / D-52).
    ///
    /// The native consumer arm: claims rows under the reserved
    /// [`KERNEL_EMBED_PLUGIN`] identity (no `tap_queue_worker`) and runs each
    /// through `run_embed_job`, which embeds the item and records its
    /// observable state. Reuses the same `claim_batch` (`FOR UPDATE SKIP
    /// LOCKED`), per-cycle fairness cap (`MAX_QUEUE_ITEMS_PER_CYCLE`), and
    /// retry/backoff/DLQ bookkeeping as the plugin arm, so embed jobs are
    /// first-class queue-v2 citizens. Width is fixed at
    /// [`KERNEL_EMBED_CONCURRENCY`] (the kernel cap ceiling) since the kernel arm
    /// has no `tap_queue_info` to declare one.
    ///
    /// A no-op when no embedding provider is wired. Safe to run concurrently
    /// with the plugin drain and with other instances — the `SKIP LOCKED` claims
    /// never double-deliver.
    pub async fn drain_embed_jobs(&self) -> Result<QueueDrainStats> {
        let mut stats = QueueDrainStats::default();
        if self.ai_providers.is_none() {
            return Ok(stats);
        }

        // Same retirement the plugin arm gets: an embed job abandoned at the
        // bound is retired rather than re-claimed forever.
        if let Err(e) = self.reap_exhausted_jobs(KERNEL_EMBED_PLUGIN).await {
            warn!(error = %e, "failed to reap exhausted embed queue items");
        }

        // The same time budget the plugin drain keeps, checked before each
        // claim. Without it the only stop was the item cap, so a slow provider
        // kept this pass claiming fresh batches until the cron run's outer
        // bound cut it off, and every job in the batch it was holding then
        // spent an attempt on work that was never allowed to finish.
        let deadline = std::time::Instant::now() + self.drain_budget;
        let mut processed: i64 = 0;
        while processed < MAX_QUEUE_ITEMS_PER_CYCLE {
            if std::time::Instant::now() >= deadline {
                debug!(
                    budget_secs = self.drain_budget.as_secs(),
                    "embed drain budget spent; remaining jobs wait for the next cycle"
                );
                break;
            }
            let batch =
                KERNEL_EMBED_CONCURRENCY.min((MAX_QUEUE_ITEMS_PER_CYCLE - processed) as usize);
            let claimed = self
                .claim_batch(KERNEL_EMBED_PLUGIN, embed_index::EMBED_QUEUE_NAME, batch)
                .await?;
            if claimed.is_empty() {
                break;
            }
            processed += claimed.len() as i64;

            // Plain spawned tasks, not a `JoinSet`: dropping a `JoinSet` aborts
            // its tasks, so a pass cancelled from outside (the cron run's bound
            // on this drain) used to abort claimed jobs part way and leave each
            // with an attempt spent and no outcome recorded. A dropped
            // `JoinHandle` only stops waiting; the job finishes and records its
            // own outcome.
            let handles: Vec<_> = claimed
                .into_iter()
                .map(|job| {
                    let pool = self.pool.clone();
                    let ai = self.ai_providers.clone();
                    let vs = self.vector_store.clone();
                    tokio::spawn(async move { run_embed_job(pool, ai, vs, job).await })
                })
                .collect();

            for handle in handles {
                match handle.await {
                    Ok(Ok(outcome)) => stats.record(outcome),
                    Ok(Err(e)) => {
                        warn!(error = %e, "embed job bookkeeping failed");
                        stats.errors += 1;
                    }
                    Err(e) => {
                        warn!(error = %e, "embed job task panicked");
                        stats.errors += 1;
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Retire rows that reached `max_attempts` without ever recording a terminal
    /// outcome, and whose claim lease has expired.
    ///
    /// [`mark_job_failed`] dead-letters on the *failure* path, which only runs
    /// when a dispatch returns. A claimer that never returns — it crashed, or it
    /// spent its entire epoch budget and its drain was torn down — leaves the row
    /// `claimed` with `attempts` at the bound and `dead_at` null. Now that
    /// [`Self::claim_batch`] refuses to re-claim such a row, something has to
    /// give it an ending, or it would sit `claimed` forever: invisible to the
    /// queue, invisible to the DLQ admin screen, and counted against nothing.
    ///
    /// Runs at the head of every drain, before any claim, so an abandoned row is
    /// retired in the same pass that would previously have re-dispatched it.
    /// Returns the number of rows retired.
    async fn reap_exhausted_jobs(&self, plugin_name: &str) -> Result<u64> {
        let now = chrono::Utc::now().timestamp();
        let reaped = sqlx::query(
            r#"
            UPDATE plugin_queue
            SET status = 'dead', dead_reason = $2, dead_at = $3,
                last_error = COALESCE(last_error, $2), locked_until = 0
            WHERE plugin_name = $1
              AND status = 'claimed'
              AND locked_until <= $3
              AND attempts >= max_attempts
            "#,
        )
        .bind(plugin_name)
        .bind(ABANDONED_CLAIM_REASON)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to retire exhausted queue items")?
        .rows_affected();

        if reaped > 0 {
            warn!(
                plugin = %plugin_name,
                count = reaped,
                "retired queue items abandoned at max attempts"
            );
        }
        Ok(reaped)
    }

    /// The worker concurrency the kernel will honor for each queue
    /// `plugin_name` declares, as the drain resolves it.
    ///
    /// The answer the drain acts on, exposed so it can be asserted and shown:
    /// before this was per queue there was no such answer to give, only a single
    /// number per plugin, and a site had no way to see that the width it
    /// declared for one queue was being applied to another.
    pub async fn resolved_queue_widths(&self, plugin_name: &str) -> BTreeMap<String, usize> {
        let Some(dispatcher) = self.tap_dispatcher.clone() else {
            return BTreeMap::new();
        };
        self.plugin_queue_widths(&dispatcher, plugin_name).await
    }

    /// List the queues of `plugin_name` that currently hold claimable rows.
    ///
    /// The drain visits queues, not plugins, so it needs the queues that exist
    /// in the table rather than only those the plugin declared: a row on a queue
    /// the plugin never declared still has to drain, or it would be stranded
    /// forever by a declaration it has no say in.
    async fn claimable_queues(&self, plugin_name: &str) -> Result<Vec<String>> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query_scalar(
            r#"
            SELECT DISTINCT queue_name
            FROM plugin_queue
            WHERE plugin_name = $1
              AND attempts < max_attempts
              AND (
                    (status = 'ready' AND next_attempt_at <= $2)
                 OR (status = 'claimed' AND locked_until <= $2)
              )
            ORDER BY queue_name
            "#,
        )
        .bind(plugin_name)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .context("failed to list claimable queues")
    }

    /// Atomically claim up to `limit` eligible items for one queue of
    /// `plugin_name`.
    ///
    /// Uses `FOR UPDATE SKIP LOCKED` so concurrent drainers (the cron drain and
    /// the resident runner, or multiple server instances) never claim the same
    /// row. Claiming sets `status = 'claimed'`, extends the lease
    /// (`locked_until`), and increments `attempts` — so a crashed claimer's
    /// attempt still counts toward `max_attempts` (bounded retries,
    /// at-least-once delivery).
    ///
    /// `attempts < max_attempts` is part of the eligibility predicate, not only
    /// of the failure bookkeeping in [`mark_job_failed`]. Without it
    /// `max_attempts` bounded nothing on the *claim* side: a row whose lease
    /// expired before any terminal outcome was recorded (the claimer crashed, or
    /// spent its whole CPU budget) was re-claimed however many times it had
    /// already been attempted, incrementing `attempts` past its own bound while
    /// `dead_at` stayed null forever. Rows that reach the bound this way are
    /// retired by [`Self::reap_exhausted_jobs`] instead.
    async fn claim_batch(
        &self,
        plugin_name: &str,
        queue_name: &str,
        limit: usize,
    ) -> Result<Vec<ClaimedJob>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now = chrono::Utc::now().timestamp();
        let lease_until = now + QUEUE_CLAIM_LEASE_SECS;
        let rows = sqlx::query(
            r#"
            UPDATE plugin_queue
            SET status = 'claimed',
                locked_until = $2,
                attempts = attempts + 1
            WHERE id IN (
                SELECT id FROM plugin_queue
                WHERE plugin_name = $1
                  AND queue_name = $5
                  AND attempts < max_attempts
                  AND (
                        (status = 'ready' AND next_attempt_at <= $3)
                     OR (status = 'claimed' AND locked_until <= $3)
                  )
                ORDER BY priority DESC, created_at ASC
                LIMIT $4
                FOR UPDATE SKIP LOCKED
            )
            RETURNING id, payload, attempts, max_attempts
            "#,
        )
        .bind(plugin_name)
        .bind(lease_until)
        .bind(now)
        .bind(limit as i64)
        .bind(queue_name)
        .fetch_all(&self.pool)
        .await
        .context("failed to claim plugin queue items")?;

        use sqlx::Row;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(ClaimedJob {
                    id: row.try_get("id").ok()?,
                    payload: row.try_get("payload").ok()?,
                    attempts: row.try_get("attempts").ok()?,
                    max_attempts: row.try_get("max_attempts").ok()?,
                })
            })
            .collect())
    }

    /// Resolve the honored worker concurrency of each queue `plugin_name`
    /// declares (D-47).
    ///
    /// Dispatches the plugin's `tap_queue_info` and reads the `concurrency` of
    /// every queue it declares, each clamped to `[1, QUEUE_CONCURRENCY_CAP]`.
    /// Read fresh each cycle, so a redeployed plugin's new declaration takes
    /// effect on the next drain.
    ///
    /// The result is a map, not a single number. It used to be a number: the
    /// maximum across the plugin's queues, applied to a claim that spanned all
    /// of them at once. A plugin declaring `analyze: 4, cluster: 1,
    /// summarize: 1` therefore ran `cluster` four wide, because the width it got
    /// was `analyze`'s. Per-queue declarations bounded nothing, which is the
    /// collapse recorded as `G-QUEUE-CONCURRENCY-COLLAPSED`.
    ///
    /// An empty map means the plugin declared nothing usable (or exports no
    /// `tap_queue_info`); its queues each drain at width 1.
    async fn plugin_queue_widths(
        &self,
        dispatcher: &Arc<TapDispatcher>,
        plugin_name: &str,
    ) -> BTreeMap<String, usize> {
        if !dispatcher
            .registry()
            .get_handlers("tap_queue_info")
            .iter()
            .any(|h| h.plugin.info.name == plugin_name)
        {
            return BTreeMap::new();
        }
        let state = self.background_state(dispatcher);
        match dispatcher
            .dispatch_to_plugin("tap_queue_info", "{}", plugin_name, state)
            .await
        {
            Some(result) => parse_queue_concurrency(&result.output),
            None => BTreeMap::new(),
        }
    }

    /// Build a background `RequestState` for kernel-internal tap dispatch.
    ///
    /// Carries the P11c / D-40 background principal so a plugin holding the
    /// `ai_background` capability may call AI from `tap_queue_worker`; the marker
    /// never reaches a web/session context.
    fn background_state(&self, dispatcher: &Arc<TapDispatcher>) -> RequestState {
        RequestState::new(
            crate::tap::UserContext::background(),
            self.background_services(dispatcher),
        )
    }

    /// The services every background dispatch from this service carries.
    ///
    /// One builder for `tap_cron` and `tap_queue_worker` both, so the per-plugin
    /// `mail` bucket and the email service cannot be attached on one background
    /// path and missing from the other; the item service and the gather service
    /// likewise.
    fn background_services(&self, dispatcher: &Arc<TapDispatcher>) -> crate::tap::RequestServices {
        let mut services = crate::tap::RequestServices::for_background(
            self.pool.clone(),
            self.ai_providers.clone(),
            self.ai_budgets.clone(),
            self.http.clone(),
        )
        .with_plugin_runtime(dispatcher.runtime().clone());
        if let Some(limiter) = self.rate_limiter.clone() {
            services = services.with_rate_limiter(limiter);
        }
        if let Some(email) = self.email.clone() {
            services = services.with_email(email);
        }
        // The `item-api` host functions decide with this. Unbound, every
        // `save-item` and `delete-item` from `tap_cron` and `tap_queue_worker`
        // was refused before the plugin's `item_background` mattered. `None`
        // only in a narrow harness that never wired one; a call then gets
        // `ERR_ITEM_SERVICE_UNAVAILABLE`.
        if let Some(items) = &self.items {
            services.set_item_service(items);
        }
        // The `query` host function runs with this. Unbound, every `query`
        // from `tap_cron` and `tap_queue_worker` answered `ERR_NO_SERVICES`
        // after the background access check had already let it through. The
        // access rules are untouched: `query` still decides the background
        // viewer and `item_background` before it reads this.
        if let Some(gather) = &self.gather {
            services.set_gather_service(gather);
        }
        services
    }

    /// Dispatch `tap_cache_purge` with `payload_json` to every enabled plugin
    /// that implements it (B303). A notification: a handler's own error is
    /// logged by the dispatcher and never reaches the caller. `false` when no
    /// plugin implements the tap (or no tap dispatcher is wired at all, as in
    /// a narrow test harness), so the background drain below can tell whether
    /// anything actually ran.
    async fn dispatch_cache_purge(&self, payload_json: &str) -> bool {
        let Some(dispatcher) = &self.tap_dispatcher else {
            return false;
        };
        if dispatcher.registry().handler_count("tap_cache_purge") == 0 {
            return false;
        }
        let state = self.background_state(dispatcher);
        let results = dispatcher
            .dispatch("tap_cache_purge", payload_json, state)
            .await;
        !results.is_empty()
    }

    /// Drain the pending edge-cache purge ([`crate::cache::purge`]) and
    /// dispatch it once, if there is anything pending: `true` when a plugin
    /// actually received it. Called every
    /// [`crate::cache::purge::DRAIN_INTERVAL`] by the background task spawned
    /// in `main.rs`, by the admin "Purge the CDN" action
    /// (`routes::admin_performance`) so a manual purge is sent at once rather
    /// than waiting out the interval, and by tests for the same reason.
    pub async fn drain_cache_purge_now(&self) -> bool {
        let Some(payload) = crate::cache::purge::take_pending_json() else {
            return false;
        };
        self.dispatch_cache_purge(&payload).await
    }

    /// Load the resident queue-runner config from `site_config` (default off).
    ///
    /// Read fresh by the cron drain (to decide whether to step aside) and by the
    /// runner loop (so a runtime disable takes effect). A read error leaves the
    /// runner off so a broken config never silently starts a scheduler.
    pub async fn queue_runner_config(&self) -> QueueRunnerConfig {
        match crate::models::SiteConfig::get(&self.pool, QUEUE_RUNNER_CONFIG_KEY).await {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_default(),
            Ok(None) => QueueRunnerConfig::default(),
            Err(e) => {
                warn!(error = %e, "failed to read queue_runner config; runner stays off");
                QueueRunnerConfig::default()
            }
        }
    }

    /// Run the resident queue-runner loop until `shutdown` fires (P11d / D-46).
    ///
    /// The opt-in half of the hybrid execution model: a boot-started background
    /// task that drains the plugin queue on its own cadence for deployments that
    /// want sub-cron latency. Re-reads the `queue_runner` config each tick so a
    /// runtime disable takes effect (and the cron drain resumes). Draining uses
    /// the same `FOR UPDATE SKIP LOCKED` claims as the cron path, so this never
    /// double-delivers with a concurrent cron pass. Returns on graceful
    /// shutdown.
    pub async fn run_queue_runner(self: Arc<Self>, shutdown: tokio_util::sync::CancellationToken) {
        let mut cfg = self.queue_runner_config().await;
        info!(
            poll_interval_secs = cfg.poll_interval_secs,
            "resident queue-runner started"
        );
        loop {
            let interval = Duration::from_secs(cfg.poll_interval_secs.max(1));
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("resident queue-runner shutting down");
                    break;
                }
                _ = tokio::time::sleep(interval) => {}
            }
            // Re-read config each tick so a runtime disable stops draining.
            cfg = self.queue_runner_config().await;
            if !cfg.enabled {
                continue;
            }
            match self.drain_plugin_queues().await {
                Ok(stats) if stats.total() > 0 || stats.errors > 0 => {
                    debug!(
                        succeeded = stats.succeeded,
                        retried = stats.retried,
                        dead = stats.dead_lettered,
                        errors = stats.errors,
                        "queue-runner drained"
                    );
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "queue-runner drain failed"),
            }
            // Drain the kernel embed queue on the same resident cadence (P11f).
            match self.drain_embed_jobs().await {
                Ok(stats) if stats.total() > 0 || stats.errors > 0 => {
                    debug!(
                        succeeded = stats.succeeded,
                        retried = stats.retried,
                        dead = stats.dead_lettered,
                        errors = stats.errors,
                        "queue-runner drained embed jobs"
                    );
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "queue-runner embed drain failed"),
            }
        }
    }

    /// Load the built-in scheduler config (B178), for display.
    ///
    /// As [`read_scheduler_config`](Self::read_scheduler_config), with a
    /// failed database read shown as the default. The scheduler loop itself
    /// does not use this: it keeps the last setting it read instead.
    pub async fn scheduler_config(&self) -> CronSchedulerConfig {
        match self.read_scheduler_config().await {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!(error = %e, "failed to read cron_interval_secs; showing the default");
                CronSchedulerConfig::default()
            }
        }
    }

    /// Read the built-in scheduler config (B178).
    ///
    /// `CRON_INTERVAL_SECS`, when set, wins outright: the process-level switch
    /// a deployment needs without a database row being right. Otherwise the
    /// `cron_interval_secs` `site_config` row, read fresh on every call so a
    /// change (including turning the scheduler off) applies without a
    /// restart. An absent or unparseable row is the default interval rather
    /// than off: unlike [`queue_runner_config`](Self::queue_runner_config),
    /// whose safe default is off, this scheduler's safe default is on, so a
    /// broken row cannot silently turn a site back into one where cron never
    /// runs. A failed read is an error, not a default, so the loop can keep
    /// the setting it last read: a database blip must not turn on a scheduler
    /// an administrator turned off.
    pub async fn read_scheduler_config(&self) -> Result<CronSchedulerConfig> {
        if let Some(interval_secs) = self.scheduler_interval_override {
            return Ok(CronSchedulerConfig {
                interval_secs,
                source: CronIntervalSource::Environment,
            });
        }
        let value = crate::models::SiteConfig::get(&self.pool, CRON_INTERVAL_CONFIG_KEY)
            .await
            .context("failed to read cron_interval_secs")?;
        Ok(match value {
            Some(value) => match CronSchedulerConfig::interval_from_value(&value) {
                Some(interval_secs) => CronSchedulerConfig {
                    interval_secs,
                    source: CronIntervalSource::SiteConfig,
                },
                None => {
                    warn!(
                        value = %value,
                        "cron_interval_secs is not a whole number of seconds; using the default"
                    );
                    CronSchedulerConfig::default()
                }
            },
            None => CronSchedulerConfig::default(),
        })
    }

    /// Run the built-in scheduler loop until `shutdown` fires (B178).
    ///
    /// Calls [`run_if_due`](Self::run_if_due) on its own cadence so a site
    /// runs cron with no external caller configured. That is the same `run`,
    /// under the same Redis lock, as `/cron/{key}` and the admin "Run now"
    /// control: whichever arrives first runs cron, and any caller that arrives
    /// while it runs gets [`CronResult::Skipped`], never a second run.
    ///
    /// The first run comes one interval after start, not at boot. After that a
    /// run is attempted once `interval_secs` have passed since this loop's
    /// last attempt finished *and* since the last run anywhere finished
    /// (`scheduled_run_due`), so an external poke or another server's
    /// scheduler that ran recently pushes this one's next run back instead of
    /// adding to it. `run_if_due` checks the second condition again once it
    /// holds the lock, so two servers that decide at the same moment still
    /// run cron once.
    ///
    /// The setting is reread every time the loop wakes, which is at most a
    /// minute apart, so a change (including turning the scheduler off, or back
    /// on) applies without a restart. A read that fails keeps the setting last
    /// read. Shutdown is honoured while a read is waiting as well as between
    /// runs, and no run starts once it has fired. A failed run is logged and
    /// the loop continues: one bad cycle must not stop the next.
    pub async fn run_scheduler(self: Arc<Self>, shutdown: tokio_util::sync::CancellationToken) {
        info!("built-in cron scheduler started");
        let mut last_attempt = tokio::time::Instant::now();
        let mut known: Option<CronSchedulerConfig> = None;
        loop {
            let tick = async {
                let read = self.read_scheduler_config().await;
                if let Err(e) = &read {
                    warn!(error = %e, "keeping the last cron scheduler setting read");
                }
                let cfg = next_scheduler_config(known, read);
                if !cfg.enabled() {
                    return (cfg, None);
                }
                let interval = Duration::from_secs(cfg.interval_secs);
                let since_attempt = last_attempt.elapsed();
                if since_attempt < interval {
                    let wait = (interval - since_attempt).as_secs_f64().ceil() as u64;
                    return (cfg, Some(Err(wait)));
                }
                let now = chrono::Utc::now().timestamp();
                let last_finished = match self.last_run().await {
                    Ok(run) => run.map(|r| r.timestamp),
                    Err(e) => {
                        debug!(error = %e, "could not read the last cron run; treating the run as due");
                        None
                    }
                };
                if scheduled_run_due(now, last_finished, cfg.interval_secs) {
                    (cfg, Some(Ok(())))
                } else {
                    // Cron ran elsewhere in this window: wait until that run
                    // is one interval old, then check again.
                    let age = now.saturating_sub(last_finished.unwrap_or(now)).max(0) as u64;
                    debug!(
                        age_secs = age,
                        interval_secs = cfg.interval_secs,
                        "scheduled cron run not due: cron ran recently elsewhere"
                    );
                    (cfg, Some(Err(cfg.interval_secs.saturating_sub(age))))
                }
            };
            let (cfg, decision) = tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                decided = tick => decided,
            };
            known = Some(cfg);
            let wait_secs = match decision {
                // Off: poll for it being turned back on.
                None => SCHEDULER_MAX_POLL_SECS,
                Some(Err(wait_secs)) => wait_secs,
                Some(Ok(())) => {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    self.run_scheduled_once(cfg.interval_secs).await;
                    last_attempt = tokio::time::Instant::now();
                    cfg.interval_secs
                }
            };
            let wait = Duration::from_secs(wait_secs.clamp(1, SCHEDULER_MAX_POLL_SECS));
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(wait) => {}
            }
        }
        info!("built-in cron scheduler stopped");
    }

    /// How many runs this process's built-in scheduler has completed.
    pub fn scheduled_runs_completed(&self) -> u64 {
        self.scheduled_runs_completed
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// One scheduled call to [`run_if_due`](Self::run_if_due), logged.
    ///
    /// The run is spawned and awaited, not awaited inline, for the same reason
    /// `/cron/{key}` spawns it: once started it finishes and releases its lock
    /// even if this loop's task is dropped part way (a runtime shutting down).
    async fn run_scheduled_once(self: &Arc<Self>, interval_secs: u64) {
        let cron = Arc::clone(self);
        let result = match tokio::spawn(async move { cron.run_if_due(interval_secs).await }).await {
            Ok(result) => result,
            Err(e) => CronResult::Failed(format!("cron task did not finish: {e}")),
        };
        match result {
            CronResult::Completed {
                tasks_run,
                duration_ms,
            } => {
                info!(duration_ms, tasks = ?tasks_run, "scheduled cron run completed");
                self.scheduled_runs_completed
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            CronResult::Skipped => {
                info!("scheduled cron run skipped: another caller ran or is running cron");
            }
            CronResult::Failed(e) => {
                warn!(error = %e, "scheduled cron run failed; the scheduler carries on");
            }
        }
    }

    /// Claim a once-a-day task across every kernel process: true for the first
    /// caller in a 24 hour window, false for the rest.
    async fn claim_daily(&self, key: &str) -> Result<bool> {
        let mut conn = self
            .redis
            .bounded_connection()
            .await
            .context("failed to get Redis connection")?;
        let claimed: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(86_400)
            .query_async(&mut conn)
            .await
            .context("failed to claim daily task")?;
        Ok(claimed.is_some())
    }

    /// Get the last cron run status.
    pub async fn last_run(&self) -> Result<Option<LastCronRun>> {
        let mut conn = self
            .redis
            .bounded_connection()
            .await
            .context("failed to get Redis connection")?;

        let data: Option<String> = conn
            .get(CRON_LAST_RUN_KEY)
            .await
            .context("failed to get last run")?;

        match data {
            Some(json) => {
                let run: LastCronRun =
                    serde_json::from_str(&json).context("failed to parse last run")?;
                Ok(Some(run))
            }
            None => Ok(None),
        }
    }

    /// Record the last cron run.
    async fn record_run(&self, result: &CronResult) -> Result<()> {
        let (status, tasks, duration_ms) = match result {
            CronResult::Completed {
                tasks_run,
                duration_ms,
            } => ("completed", tasks_run.clone(), Some(*duration_ms)),
            CronResult::Skipped => ("skipped", Vec::new(), None),
            CronResult::Failed(_) => ("failed", Vec::new(), None),
        };
        let run = LastCronRun {
            timestamp: chrono::Utc::now().timestamp(),
            hostname: hostname(),
            result: format!("{result:?}"),
            status: status.to_string(),
            tasks,
            duration_ms,
        };

        let mut conn = self
            .redis
            .bounded_connection()
            .await
            .context("failed to get Redis connection")?;

        let json = serde_json::to_string(&run).context("failed to serialize run")?;
        conn.set_ex::<_, _, ()>(CRON_LAST_RUN_KEY, &json, 86400)
            .await
            .context("failed to record run")?;

        Ok(())
    }
}

/// Last cron run information.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LastCronRun {
    pub timestamp: i64,
    pub hostname: String,
    pub result: String,
    /// `completed`, `skipped` or `failed`. Empty in a record written before
    /// these fields existed.
    #[serde(default)]
    pub status: String,
    /// The tasks a completed run reported, each as `name: count`.
    #[serde(default)]
    pub tasks: Vec<String>,
    /// How long a completed run took.
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// Await one cron task, for at most `limit`.
///
/// On timeout the task's future is dropped, the timeout is logged at warn with
/// the task's name, `<task>:TIMEOUT` goes into the run's task list as it always
/// has for `tap_cron`, and `None` comes back so the run moves on to its next
/// task. See [`CRON_TASK_TIMEOUT_SECS`].
async fn bounded<T>(
    task: &str,
    limit: Duration,
    tasks_run: &mut Vec<String>,
    fut: impl std::future::Future<Output = T>,
) -> Option<T> {
    match tokio::time::timeout(limit, fut).await {
        Ok(value) => Some(value),
        Err(_) => {
            warn!(
                task,
                timeout_secs = limit.as_secs_f64(),
                "cron task timed out; moving on to the next task"
            );
            tasks_run.push(format!("{task}:TIMEOUT"));
            None
        }
    }
}

/// Whether this process has a Pagefind rebuild in flight.
static PAGEFIND_REBUILDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The one Pagefind rebuild this process may have running at a time.
///
/// A cron run waits on a rebuild for one task's bound and then leaves it running
/// in the background (see `run`), so the next run can arrive while it is still
/// building. This keeps that run from starting a second build that would only
/// lose the publish race to the first. Correctness does not rest on it: the
/// advisory lock and generation check in `search_update` serialize every
/// publisher across processes. Dropped when the rebuild ends, however it ends.
struct PagefindRebuild;

impl PagefindRebuild {
    /// Claim the slot, or `None` if a rebuild is already running.
    fn start() -> Option<Self> {
        PAGEFIND_REBUILDING
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok()
            .map(|_| Self)
    }
}

impl Drop for PagefindRebuild {
    fn drop(&mut self) {
        PAGEFIND_REBUILDING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// When the run holding a cron lock took it, from the lock's value, in unix
/// seconds. `None` for a value written without [`LOCK_SINCE_MARKER`].
fn lock_held_since(value: &str) -> Option<i64> {
    value
        .rsplit_once(LOCK_SINCE_MARKER)
        .and_then(|(_, since)| since.parse().ok())
}

/// What a run that found the cron lock held should make of it.
#[derive(Debug, PartialEq, Eq)]
enum HeldLock {
    /// Another run is going and has not been at it unusually long. `held_secs`
    /// is `None` when the holder's value records no start time.
    Overlap { held_secs: Option<i64> },
    /// The holder has had the lock longer than [`MAX_LOCK_RENEWAL_SECS`], the
    /// bound past which no healthy run renews it: whatever holds it is stuck.
    TooLong { held_secs: i64 },
    /// The lock has no TTL, so it will never free itself.
    NoExpiry,
}

/// Classify a held cron lock from its value, its TTL (`TTL` semantics: `-1`
/// means no expiry) and the time now.
fn classify_held_lock(value: &str, ttl_secs: i64, now: i64) -> HeldLock {
    if ttl_secs == -1 {
        return HeldLock::NoExpiry;
    }
    let held_secs = lock_held_since(value).map(|since| (now - since).max(0));
    match held_secs {
        Some(held) if held > MAX_LOCK_RENEWAL_SECS as i64 => HeldLock::TooLong { held_secs: held },
        _ => HeldLock::Overlap { held_secs },
    }
}

/// What [`run_heartbeat`] is allowed to do, and for how long.
///
/// Every value used to be a module constant read at the point of use, which made
/// the renewal bound impossible to exercise without waiting out the real one.
#[derive(Debug, Clone)]
pub(crate) struct HeartbeatConfig {
    /// Redis key holding the lock.
    pub key: String,
    /// How often to attempt a renewal.
    pub interval: Duration,
    /// TTL written by each renewal.
    pub ttl_secs: u64,
    /// How long this run may go on renewing before it must stop.
    pub max_renewal: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            key: CRON_LOCK_KEY.to_string(),
            interval: Duration::from_secs(HEARTBEAT_INTERVAL_SECS),
            ttl_secs: LOCK_TTL_SECS,
            max_renewal: Duration::from_secs(MAX_LOCK_RENEWAL_SECS),
        }
    }
}

/// Run the heartbeat task to extend lock TTL, for as long as the run is entitled
/// to hold it.
///
/// The entitlement is the point. This used to extend the lock every
/// `HEARTBEAT_INTERVAL_SECS` for as long as [`CronService::run`] had not
/// returned, with no condition attached, which made a stuck run
/// indistinguishable from a working one and actively hid it: the lock's TTL sat
/// at its maximum and *rose* between samples precisely because the run was not
/// finishing. Every later trigger answered "another instance is running cron", so
/// `tap_cron` never dispatched again, and anything reporting on queue health from
/// `tap_cron` went quiet at exactly the moment it was needed.
///
/// Renewal stops after [`MAX_LOCK_RENEWAL_SECS`]. The lock then expires on its
/// own TTL, the next trigger proceeds, and that reporting resumes. Nothing is
/// killed and nothing is restarted: a stuck run continues, it simply stops being
/// able to keep the whole cron system waiting on it. `run` still releases the
/// lock when it finally returns, and the release is owner-checked, so a run that
/// outlived its renewal cannot delete a newer run's lock.
pub(crate) async fn run_heartbeat(
    redis: RedisClient,
    lock_value: &str,
    mut stop_rx: watch::Receiver<bool>,
    cfg: HeartbeatConfig,
) {
    let mut interval = tokio::time::interval(cfg.interval);
    let started = std::time::Instant::now();
    let mut gave_up = false;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if gave_up {
                    continue;
                }
                if started.elapsed() >= cfg.max_renewal {
                    warn!(
                        held_secs = started.elapsed().as_secs(),
                        max_secs = cfg.max_renewal.as_secs(),
                        "cron run has held the lock too long; no longer extending it so \
                         the next run can proceed"
                    );
                    gave_up = true;
                    continue;
                }
                if let Ok(mut conn) = redis.bounded_connection().await {
                    // Extend lock TTL if we still own it
                    let script = redis::Script::new(EXTEND_LOCK_SCRIPT);
                    if let Err(e) = script
                        .key(&cfg.key)
                        .arg(lock_value)
                        .arg(cfg.ttl_secs)
                        .invoke_async::<()>(&mut conn)
                        .await
                    {
                        warn!(error = %e, "failed to extend lock TTL");
                    } else {
                        debug!("extended cron lock TTL");
                    }
                }
            }
            changed = stop_rx.changed() => {
                // An error here means every sender is gone, and the only sender
                // is the local in `CronService::run` that spawned this task. So
                // the run is over — and crucially, it is over in the one way it
                // cannot tell us about: its future was dropped part way, which
                // is what happens whenever the HTTP client that triggered cron
                // hangs up (the poker gives each run 30 seconds). Treating that
                // as "nothing to do" left this arm permanently ready, because a
                // closed channel reports closed every time it is asked, and the
                // `loop` re-entered `select!` to be told again: a tight busy
                // loop on a runtime worker, still renewing the lock every tick,
                // that no longer had a run to stop it and so outlived every
                // later cron trigger until the process was restarted.
                //
                // There is nothing left to hold the lock for either way, so
                // stop. The lock then expires on its own TTL.
                if changed.is_err() {
                    debug!("heartbeat stopping: its cron run is gone");
                    break;
                }
                if *stop_rx.borrow() {
                    debug!("heartbeat stopping");
                    break;
                }
            }
        }
    }
}

/// Build a shared HTTP client for plugin outbound requests (queue-worker and
/// cron dispatch).
///
/// Built from the kernel's SSRF-hardened outbound builder (rebinding-safe
/// resolver + per-hop redirect revalidation, p11i) plus a User-Agent header so
/// downstream servers can identify the traffic source (required by the GitHub
/// raw API, among others).
///
/// # Panics
///
/// Panics if the TLS backend fails to initialize — fail-closed rather than fall
/// back to an unhardened client (see [`crate::host::http::build_outbound_client`]).
fn build_http_client() -> reqwest::Client {
    // Fail-closed: never silently fall back to an unhardened client. `build()`
    // only fails on TLS backend init, which would also fail `Client::new()`.
    #[allow(clippy::expect_used)]
    crate::host::http::hardened_outbound_builder()
        .user_agent(format!("Trovato/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("SSRF-hardened cron/queue HTTP client must build")
}

/// A fresh owner value for the cron lock: `host:pid:uuid:t<unix seconds>`,
/// unique per run, and recording when the run took the lock (see
/// [`LOCK_SINCE_MARKER`]). The uuid is plain hex, so the marker stays
/// unambiguous.
fn new_lock_value() -> String {
    format!(
        "{}:{}:{}{LOCK_SINCE_MARKER}{}",
        hostname(),
        std::process::id(),
        uuid::Uuid::new_v4().simple(),
        chrono::Utc::now().timestamp()
    )
}

/// Get hostname for lock identification.
fn hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Lua script to release lock only if we own it.
const RELEASE_LOCK_SCRIPT: &str = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("DEL", KEYS[1])
else
    return 0
end
"#;

/// Lua script to extend lock TTL only if we own it.
const EXTEND_LOCK_SCRIPT: &str = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("EXPIRE", KEYS[1], ARGV[2])
else
    return 0
end
"#;

impl std::fmt::Debug for CronService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronService").finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A gather service on a database that is never reached: the binding is
    /// what is under test, not a read.
    fn unreached_gather() -> Arc<GatherService> {
        use crate::gather::{
            CategoryService, GatherAccessConfig, GatherConfig, GatherExtensionRegistry,
        };
        let pool = PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").expect("lazy pool");
        let metrics = Arc::new(crate::metrics::Metrics::new());
        GatherService::new(
            pool.clone(),
            CategoryService::new(pool, Duration::from_secs(60), metrics.clone()),
            Arc::new(GatherExtensionRegistry::new()),
            GatherConfig {
                ttl: Duration::from_secs(60),
                max_page_size: 100,
                access: GatherAccessConfig::default(),
            },
            None,
            None,
            metrics,
        )
    }

    /// The services `tap_cron` and `tap_queue_worker` dispatch with carry the
    /// gather service `CronService` was given, and the background state built
    /// from them carries it too. Before B249 the builder bound the item
    /// service only, so `query` from either path answered `ERR_NO_SERVICES`.
    #[tokio::test]
    async fn background_services_carry_the_gather_service() {
        use crate::plugin::{PluginConfig, PluginRuntime};
        use crate::tap::TapRegistry;

        let pool = PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").expect("lazy pool");
        let redis = RedisClient::open("redis://127.0.0.1:1/").expect("redis client");
        let runtime = PluginRuntime::new(&PluginConfig::default()).expect("plugin runtime");
        let registry = Arc::new(TapRegistry::from_plugins(&runtime));
        let dispatcher = Arc::new(TapDispatcher::new(Arc::new(runtime), registry));

        let mut cron = CronService::new(redis, pool);
        assert!(
            cron.background_services(&dispatcher)
                .gather_service()
                .is_none(),
            "a harness that never wired gather carries none"
        );

        let gather = unreached_gather();
        cron.set_gather_service(gather.clone());
        let bound = cron
            .background_services(&dispatcher)
            .gather_service()
            .expect("the background services carry the gather service");
        assert!(
            Arc::ptr_eq(&bound, &gather),
            "the one gather service, not a copy"
        );

        let state = cron.background_state(&dispatcher);
        assert!(state.user.is_background(), "still the background principal");
        let carried = state
            .services()
            .and_then(|services| services.gather_service())
            .expect("the background state carries the gather service");
        assert!(Arc::ptr_eq(&carried, &gather));
    }

    #[test]
    fn test_hostname() {
        let h = hostname();
        assert!(!h.is_empty());
    }

    #[test]
    fn test_last_cron_run_serde() {
        let run = LastCronRun {
            timestamp: 1234567890,
            hostname: "test-host".to_string(),
            result: "Completed".to_string(),
            ..Default::default()
        };

        let json = serde_json::to_string(&run).unwrap();
        let parsed: LastCronRun = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.hostname, "test-host");
    }

    // ── P11d: queue v2 pure-logic units (D-47 concurrency, backoff) ──────────

    #[test]
    fn parse_queue_concurrency_reads_each_queue() {
        // Ritrovo-style single-queue declaration.
        let one = parse_queue_concurrency(r#"[{"name":"ritrovo_import","concurrency":4}]"#);
        assert_eq!(one.get("ritrovo_import").copied(), Some(4));

        // Multiple queues: each keeps its OWN width. The old parser collapsed
        // these to the maximum (7) and applied it to every queue, which is the
        // per-queue collapse this replaces.
        let many = parse_queue_concurrency(
            r#"[{"name":"a","concurrency":2},{"name":"b","concurrency":3}]"#,
        );
        assert_eq!(many.get("a").copied(), Some(2));
        assert_eq!(many.get("b").copied(), Some(3));
        assert_eq!(many.len(), 2);
    }

    #[test]
    fn parse_queue_concurrency_defaults_to_one() {
        // Empty declaration, non-array and garbage name no queues at all.
        assert!(parse_queue_concurrency("[]").is_empty());
        assert!(parse_queue_concurrency(r#"{"not":"an array"}"#).is_empty());
        assert!(parse_queue_concurrency("not json at all").is_empty());
        // An entry with no usable name bounds no queue, so it is dropped.
        assert!(parse_queue_concurrency(r#"[{"concurrency":3}]"#).is_empty());
        assert!(parse_queue_concurrency(r#"[{"name":"","concurrency":3}]"#).is_empty());
        // A named queue with a missing or zero width falls back to 1.
        assert_eq!(
            parse_queue_concurrency(r#"[{"name":"a"}]"#)
                .get("a")
                .copied(),
            Some(1)
        );
        assert_eq!(
            parse_queue_concurrency(r#"[{"name":"a","concurrency":0}]"#)
                .get("a")
                .copied(),
            Some(1)
        );
    }

    #[test]
    fn concurrency_is_clamped_to_the_kernel_cap() {
        // D-47: a plugin may declare any concurrency; the parse clamps it to the
        // kernel cap. This is the "clamping is enforced" contract, and it now
        // holds per queue rather than once per plugin.
        assert_eq!(QUEUE_CONCURRENCY_CAP, 4);
        let widths = parse_queue_concurrency(
            r#"[{"name":"big","concurrency":64},{"name":"small","concurrency":2}]"#,
        );
        // Over the cap is lowered to the cap.
        assert_eq!(widths.get("big").copied(), Some(4));
        // Under the cap is honored, not raised to it.
        assert_eq!(widths.get("small").copied(), Some(2));
    }

    #[test]
    fn a_contradictory_duplicate_keeps_the_lower_width() {
        // Two entries naming the same queue is a malformed declaration. Taking
        // the lower of the two is the reading that cannot over-dispatch.
        let widths = parse_queue_concurrency(
            r#"[{"name":"q","concurrency":4},{"name":"q","concurrency":1}]"#,
        );
        assert_eq!(widths.get("q").copied(), Some(1));
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        // attempts is the post-claim count (>= 1): 60, 120, 240, ...
        assert_eq!(backoff_secs(1), QUEUE_BACKOFF_BASE_SECS);
        assert_eq!(backoff_secs(2), QUEUE_BACKOFF_BASE_SECS * 2);
        assert_eq!(backoff_secs(3), QUEUE_BACKOFF_BASE_SECS * 4);
        // Never exceeds the ceiling, and never overflows on absurd inputs.
        assert_eq!(backoff_secs(1000), QUEUE_BACKOFF_CAP_SECS);
        assert!(backoff_secs(i32::MAX) <= QUEUE_BACKOFF_CAP_SECS);
    }

    #[test]
    fn queue_runner_config_defaults_to_off() {
        let cfg = QueueRunnerConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 5);
        // A bare `{}` from site_config deserializes to the same safe default.
        let from_empty: QueueRunnerConfig = serde_json::from_str("{}").unwrap();
        assert!(!from_empty.enabled);
        assert_eq!(from_empty.poll_interval_secs, 5);
    }

    /// The lock value carries its start time, and only a value written with
    /// the marker has an age: an older kernel's `host:pid` must not have its
    /// pid read as a timestamp.
    #[test]
    fn a_lock_value_records_when_it_was_taken() {
        assert_eq!(lock_held_since("web-1:42:t1700000000"), Some(1_700_000_000));
        assert_eq!(lock_held_since("web-1:42"), None);
        assert_eq!(lock_held_since("web-1:42:tsoon"), None);
        // The value a run writes now, and the uuid-only form the release
        // before it wrote.
        let now = chrono::Utc::now().timestamp();
        let since = lock_held_since(&new_lock_value()).expect("a fresh value has an age");
        assert!((now - 1..=now + 1).contains(&since), "{since} vs {now}");
        assert_eq!(
            lock_held_since("web-1:42:0123456789abcdef0123456789abcdef"),
            None
        );
        assert_ne!(new_lock_value(), new_lock_value());
    }

    #[test]
    fn only_a_lock_held_past_the_renewal_bound_is_too_long() {
        let now = 1_700_010_000;
        let since = |held: i64| format!("web-1:42:t{}", now - held);
        let max = MAX_LOCK_RENEWAL_SECS as i64;

        assert_eq!(
            classify_held_lock(&since(5), 295, now),
            HeldLock::Overlap { held_secs: Some(5) }
        );
        assert_eq!(
            classify_held_lock(&since(max), 300, now),
            HeldLock::Overlap {
                held_secs: Some(max)
            }
        );
        assert_eq!(
            classify_held_lock(&since(max + 1), 300, now),
            HeldLock::TooLong { held_secs: max + 1 }
        );
        // An older kernel's value has no age; a TTL still bounds it.
        assert_eq!(
            classify_held_lock("web-1:42", 120, now),
            HeldLock::Overlap { held_secs: None }
        );
        // No expiry is never an ordinary overlap, whatever the age.
        assert_eq!(classify_held_lock(&since(1), -1, now), HeldLock::NoExpiry);
    }

    // ── B178: the built-in scheduler ─────────────────────────────────────────

    #[test]
    fn cron_scheduler_config_defaults_to_on() {
        // Opposite of the queue runner's safe default: a fresh install must run
        // cron with no external caller, so the reading of a missing setting is
        // the default interval, not off.
        let cfg = CronSchedulerConfig::default();
        assert!(cfg.enabled());
        assert_eq!(cfg.interval_secs, DEFAULT_CRON_INTERVAL_SECS);
        assert_eq!(cfg.source, CronIntervalSource::Default);
    }

    #[test]
    fn cron_scheduler_config_zero_means_off() {
        let cfg = CronSchedulerConfig {
            interval_secs: 0,
            source: CronIntervalSource::SiteConfig,
        };
        assert!(!cfg.enabled());
    }

    #[test]
    fn cron_interval_reads_a_number_or_a_numeric_string() {
        use serde_json::json;
        assert_eq!(
            CronSchedulerConfig::interval_from_value(&json!(60)),
            Some(60)
        );
        assert_eq!(CronSchedulerConfig::interval_from_value(&json!(0)), Some(0));
        assert_eq!(
            CronSchedulerConfig::interval_from_value(&json!(" 90 ")),
            Some(90)
        );
        assert_eq!(CronSchedulerConfig::interval_from_value(&json!(-5)), None);
        assert_eq!(CronSchedulerConfig::interval_from_value(&json!(1.5)), None);
        assert_eq!(
            CronSchedulerConfig::interval_from_value(&json!("soon")),
            None
        );
        assert_eq!(CronSchedulerConfig::interval_from_value(&json!({})), None);
    }

    #[test]
    fn a_scheduled_run_waits_out_a_recent_run_from_any_caller() {
        // Nothing has run: due.
        assert!(scheduled_run_due(1_000, None, 300));
        // Another server's scheduler or an external poke finished 100 s ago:
        // not due, so N servers do not run cron N times per interval.
        assert!(!scheduled_run_due(1_000, Some(900), 300));
        // Exactly one interval ago: due.
        assert!(scheduled_run_due(1_000, Some(700), 300));
        // A record from a clock ahead of ours: not due until it is an interval old.
        assert!(!scheduled_run_due(1_000, Some(1_010), 300));
    }

    #[test]
    fn a_failed_settings_read_keeps_the_last_setting() {
        let off = CronSchedulerConfig {
            interval_secs: 0,
            source: CronIntervalSource::SiteConfig,
        };
        let hourly = CronSchedulerConfig {
            interval_secs: 3600,
            source: CronIntervalSource::SiteConfig,
        };
        let failed = || Err(anyhow::anyhow!("connection refused"));
        // Turned off, then the database blips: still off, not the default.
        let cfg = next_scheduler_config(Some(off), failed());
        assert!(!cfg.enabled(), "{cfg:?}");
        assert_eq!(next_scheduler_config(Some(hourly), failed()), hourly);
        // A successful read always wins.
        assert_eq!(next_scheduler_config(Some(off), Ok(hourly)), hourly);
        // Nothing read yet: the default.
        assert_eq!(
            next_scheduler_config(None, failed()),
            CronSchedulerConfig::default()
        );
    }

    /// The scheduler's run rechecks that cron is due once it holds the lock:
    /// a run that finished in between (another server's, deciding from the
    /// same old `cron:last_run`) makes it give the lock back and skip.
    #[tokio::test]
    async fn a_scheduled_run_skips_when_cron_finished_after_it_decided() {
        let Ok(client) = redis::Client::open(trovato_test_utils::env::redis_url()) else {
            return;
        };
        let Ok(mut conn) = client.bounded_connection().await else {
            return; // No Redis in this environment.
        };
        // A pool that is never used: the skip happens before any task runs.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .expect("lazy pool");
        let cron = CronService::new(client, pool);
        // Another run finished just now. Writing the site-wide record is safe
        // here: it is what every completed run writes.
        let now = chrono::Utc::now().timestamp();
        let record = serde_json::json!({
            "timestamp": now, "hostname": "elsewhere", "result": "Completed",
            "status": "completed", "tasks": [], "duration_ms": 1
        })
        .to_string();
        let _: () = conn
            .set_ex(super::CRON_LAST_RUN_KEY, &record, 86_400)
            .await
            .expect("record a fresh run");

        match cron.run_if_due(3600).await {
            // Another process held the lock: also not a second run.
            CronResult::Skipped => {}
            other => panic!("a run that is no longer due executed: {other:?}"),
        }
        let held: Option<String> = conn.get(CRON_LOCK_KEY).await.unwrap_or(None);
        assert!(
            held.is_none_or(|v| !v.starts_with(&format!("{}:{}:", hostname(), std::process::id()))),
            "the skipped run kept the cron lock"
        );
    }

    #[test]
    fn every_cron_run_owns_a_distinct_lock_value() {
        // `host:pid` alone made two runs in one process indistinguishable to
        // the owner checks in `release_lock` and the heartbeat.
        let a = new_lock_value();
        let b = new_lock_value();
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("{}:{}:", hostname(), std::process::id())));
    }

    /// A run that outlived its lock must not release the lock a newer run in
    /// the same process has since taken. Red while the value was `host:pid`:
    /// both runs wrote the same value, so the stale run's owner-checked
    /// release matched and deleted the newer run's lock.
    #[tokio::test]
    async fn a_stale_run_cannot_release_a_newer_runs_lock() {
        let Ok(client) = redis::Client::open(trovato_test_utils::env::redis_url()) else {
            return;
        };
        let Ok(mut conn) = client.bounded_connection().await else {
            return; // No Redis in this environment; uniqueness is covered above.
        };
        let key = format!("cron:lock:test:{}", uuid::Uuid::new_v4());
        let stale = new_lock_value();
        let newer = new_lock_value();
        // The stale run's lock expired and the newer run took it.
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(&newer)
            .arg("EX")
            .arg(60)
            .query_async(&mut conn)
            .await
            .expect("seed the newer run's lock");

        redis::Script::new(RELEASE_LOCK_SCRIPT)
            .key(&key)
            .arg(&stale)
            .invoke_async::<()>(&mut conn)
            .await
            .expect("run the release script");

        let held: Option<String> = conn.get(&key).await.expect("read the lock");
        let _: () = conn.del(&key).await.unwrap_or(());
        assert_eq!(
            held.as_deref(),
            Some(newer.as_str()),
            "the stale run's release deleted the newer run's lock"
        );
    }

    #[test]
    fn drain_stats_totals() {
        let mut s = QueueDrainStats::default();
        s.record(JobOutcome::Succeeded);
        s.record(JobOutcome::Succeeded);
        s.record(JobOutcome::Retried);
        s.record(JobOutcome::DeadLettered);
        assert_eq!(s.succeeded, 2);
        assert_eq!(s.retried, 1);
        assert_eq!(s.dead_lettered, 1);
        assert_eq!(s.total(), 4);
    }

    // ── CPU exhaustion is told apart from an ordinary failure by duration ────

    // ── The heartbeat stops renewing ────────────────────────────────────────

    /// The renewal bound: a run that will not finish must stop being able to
    /// keep the cron lock alive.
    ///
    /// Before this, `run_heartbeat` extended the lock on every tick for as long
    /// as the run had not returned, with no condition attached — so a stuck run
    /// held the lock forever, every later trigger answered "another instance is
    /// running cron", and the TTL rose between samples *because* the run was
    /// stuck. The test drives a tiny interval and a tiny maximum and asserts the
    /// renewals stop: with no bound the count grows without limit.
    #[tokio::test]
    async fn the_heartbeat_stops_renewing_past_its_maximum() {
        let Ok(client) = redis::Client::open(trovato_test_utils::env::redis_url()) else {
            return;
        };
        let Ok(mut conn) = client.bounded_connection().await else {
            return; // No Redis in this environment; the bound is covered above.
        };

        let key = format!("cron:lock:test:{}", uuid::Uuid::new_v4());
        let value = "owner";
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(value)
            .arg("EX")
            .arg(60)
            .query_async(&mut conn)
            .await
            .expect("seed the lock");

        let (stop_tx, stop_rx) = watch::channel(false);
        let cfg = HeartbeatConfig {
            key: key.clone(),
            interval: Duration::from_millis(50),
            ttl_secs: 60,
            max_renewal: Duration::from_millis(250),
        };
        let handle = tokio::spawn({
            let client = client.clone();
            async move {
                run_heartbeat(client, value, stop_rx, cfg).await;
            }
        });

        // Well past max_renewal: an unbounded heartbeat would still be renewing.
        tokio::time::sleep(Duration::from_millis(900)).await;

        // Shrink the TTL by hand. A heartbeat still renewing would push it back
        // up to 60 within two ticks (100 ms); one that has given up leaves it.
        let _: () = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(5)
            .query_async(&mut conn)
            .await
            .expect("shrink the ttl");
        tokio::time::sleep(Duration::from_millis(300)).await;

        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .expect("read the ttl");

        let _ = stop_tx.send(true);
        let _ = handle.await;
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap_or(());

        assert!(
            ttl <= 5,
            "the heartbeat pushed the TTL back to {ttl} after its renewal maximum: \
             a run that cannot finish is still keeping the cron lock alive"
        );
    }

    /// A heartbeat whose run was cancelled must end, not spin.
    ///
    /// `CronService::run` owns the only sender. When its future is dropped part
    /// way — which is what happens every time the HTTP client that triggered
    /// cron hangs up, and the poker allows each run 30 seconds — the sender goes
    /// with it and `changed()` reports a closed channel immediately, and for
    /// ever after. The arm used to discard that result and fall through, leaving
    /// itself permanently ready, so the `loop` re-entered `select!` to be told
    /// again: a busy loop pinning a runtime worker at a full core, still
    /// renewing the cron lock on every tick, with no run left to stop it. Only
    /// a restart cleared it.
    ///
    /// No Redis is needed: the interval is set long enough that the renewing arm
    /// never fires, so the test turns entirely on the closed channel.
    #[tokio::test]
    async fn the_heartbeat_stops_when_its_run_is_gone() {
        let client = redis::Client::open("redis://127.0.0.1:1/").expect("lazy client");
        let (stop_tx, stop_rx) = watch::channel(false);
        let cfg = HeartbeatConfig {
            key: "cron:lock:test:cancelled".to_string(),
            interval: Duration::from_secs(3600),
            ttl_secs: 60,
            max_renewal: Duration::from_secs(3600),
        };
        let handle = tokio::spawn(async move {
            run_heartbeat(client, "owner", stop_rx, cfg).await;
        });

        // The run is cancelled: its future, and the sender it held, are dropped
        // without anyone ever sending `true`.
        drop(stop_tx);

        let finished = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            finished.is_ok(),
            "the heartbeat outlived the run that owned it: with the sender gone \
             it must stop, not spin on a closed channel for ever"
        );
    }
}
