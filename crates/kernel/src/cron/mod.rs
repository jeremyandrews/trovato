//! Scheduled operations and background tasks.
//!
//! Provides distributed cron with Redis-based locking to ensure
//! exactly-once execution across multiple server instances.

mod pagefind;
mod queue;
mod tasks;

pub use queue::{Queue, RedisQueue};
pub use tasks::CronTasks;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use redis::{AsyncCommands, Client as RedisClient};
use sqlx::PgPool;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::file::FileService;
use crate::services::ai_provider::AiProviderService;
use crate::services::ai_token_budget::AiTokenBudgetService;
use crate::services::embed_index::{
    self, EmbedJobPayload, KERNEL_EMBED_CONCURRENCY, KERNEL_EMBED_PLUGIN,
};
use crate::services::vector_store::{PgVectorStore, VectorStore};
use crate::tap::{RequestState, TapDispatcher};

/// Lock TTL in seconds (5 minutes).
const LOCK_TTL_SECS: u64 = 300;

/// Heartbeat interval in seconds (60 seconds).
const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// Cron lock key in Redis.
const CRON_LOCK_KEY: &str = "cron:lock";

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
#[derive(Debug)]
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
/// (the default) keeps the external-cron cadence contract unchanged. When
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

/// Exponential backoff for a failed attempt: `BASE * 2^(attempts-1)`, capped at
/// [`QUEUE_BACKOFF_CAP_SECS`]. `attempts` is the post-claim count (>= 1).
fn backoff_secs(attempts: i32) -> i64 {
    let exp = attempts.saturating_sub(1).clamp(0, 16) as u32;
    QUEUE_BACKOFF_BASE_SECS
        .saturating_mul(1i64 << exp)
        .min(QUEUE_BACKOFF_CAP_SECS)
}

/// Extract the maximum declared `concurrency` from a `tap_queue_info` result.
///
/// `tap_queue_info` returns a JSON array of
/// `{ "name": string, "concurrency": int }`. Missing/invalid entries contribute
/// nothing; an empty or unparseable declaration yields 1.
fn parse_max_concurrency(output: &str) -> usize {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return 1;
    };
    let Some(queues) = value.as_array() else {
        return 1;
    };
    queues
        .iter()
        .filter_map(|q| q.get("concurrency").and_then(serde_json::Value::as_u64))
        .map(|c| c as usize)
        .max()
        .filter(|&c| c >= 1)
        .unwrap_or(1)
}

/// Dispatch `tap_queue_worker` for one claimed job and record its terminal
/// outcome (P11d). Runs as an independent task so a plugin's declared
/// concurrency actually executes in parallel.
///
/// Success (`tap_queue_worker` returns) deletes the row. Failure (the worker
/// traps or returns an error result, surfaced as `None` from
/// `dispatch_to_plugin`) either reschedules with backoff or, at
/// `max_attempts`, dead-letters the row with the last error preserved.
#[allow(clippy::too_many_arguments)]
async fn run_queue_job(
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    ai_providers: Option<Arc<AiProviderService>>,
    ai_budgets: Option<Arc<AiTokenBudgetService>>,
    http: reqwest::Client,
    plugin_name: String,
    job: ClaimedJob,
) -> Result<JobOutcome> {
    // Infallible: payload came from JSONB and round-trips to a string.
    let input_json = serde_json::to_string(&job.payload).unwrap_or_else(|_| "{}".to_string());

    // P11c / D-40: queue-worker dispatch carries the kernel-internal background
    // principal so a plugin holding `ai_background` may call AI from the worker.
    let state = RequestState::new(
        crate::tap::UserContext::background(),
        crate::tap::RequestServices::for_background(pool.clone(), ai_providers, ai_budgets, http)
            .with_plugin_runtime(dispatcher.runtime().clone()),
    );

    let dispatched = dispatcher
        .dispatch_to_plugin("tap_queue_worker", &input_json, &plugin_name, state)
        .await;

    if dispatched.is_some() {
        mark_job_succeeded(&pool, job.id).await?;
        return Ok(JobOutcome::Succeeded);
    }

    // Failure: the worker trapped or returned an error result.
    let err = "tap_queue_worker failed (trap or error result)";
    mark_job_failed(&pool, &job, &plugin_name, err).await
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
    match ai.embed(&text).await {
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
    /// Base directory the generated Pagefind index is written into.
    ///
    /// Defaults to `./static` so a harness without a `Config` still has a
    /// destination; `apply_runtime_config` sets it from the static search path.
    pagefind_static_dir: PathBuf,
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
            pagefind_enabled: false,
            pagefind_static_dir: PathBuf::from("./static"),
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
            pagefind_enabled: false,
            pagefind_static_dir: PathBuf::from("./static"),
        }
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

    /// Enable pagefind index rebuilding (requires `trovato_search` plugin).
    pub fn set_pagefind_enabled(&mut self, enabled: bool) {
        self.pagefind_enabled = enabled;
    }

    /// Apply the configuration the cron tasks used to read from the environment.
    ///
    /// The Pagefind index needs one destination, so it goes into the **base**
    /// (first) static directory: writing it into every root would leave several
    /// copies of a generated artifact to keep in step, and writing it into the
    /// last root would mean a cron job dropping build output into an
    /// application's own repository.
    pub fn apply_runtime_config(&mut self, runtime: &crate::config::RuntimeConfig) {
        self.tasks
            .set_security_audit_retention_days(runtime.security_audit_retention_days);
        if let Some(base) = runtime.static_dirs.first() {
            self.pagefind_static_dir = base.clone();
        }
    }

    /// Set optional plugin services for cron tasks.
    pub fn set_plugin_services(
        &mut self,
        content_lock: Option<std::sync::Arc<crate::services::content_lock::ContentLockService>>,
        audit: Option<std::sync::Arc<crate::services::audit::AuditService>>,
    ) {
        self.tasks.set_plugin_services(content_lock, audit);
    }

    /// Set the email service for sending queued emails.
    pub fn set_email_service(
        &mut self,
        email: Option<std::sync::Arc<crate::services::email::EmailService>>,
    ) {
        self.tasks.set_email_service(email);
    }

    /// Run all cron tasks.
    ///
    /// Acquires a distributed lock before running to ensure only one
    /// instance executes cron at a time.
    pub async fn run(&self) -> CronResult {
        let start = std::time::Instant::now();

        // Try to acquire lock
        let lock_value = match self.acquire_lock().await {
            Ok(Some(v)) => v,
            Ok(None) => {
                debug!("cron lock held by another instance, skipping");
                return CronResult::Skipped;
            }
            Err(e) => {
                warn!(error = %e, "failed to acquire cron lock");
                return CronResult::Failed(e.to_string());
            }
        };

        info!("acquired cron lock, running tasks");

        // Start heartbeat task
        let (stop_tx, stop_rx) = watch::channel(false);
        let heartbeat_redis = self.redis.clone();
        let heartbeat_lock = lock_value.clone();
        let heartbeat_handle = tokio::spawn(async move {
            run_heartbeat(heartbeat_redis, &heartbeat_lock, stop_rx).await;
        });

        // Run tasks
        let mut tasks_run = Vec::new();

        // Cleanup temporary files
        match self.tasks.cleanup_temp_files().await {
            Ok(count) => {
                info!(deleted = count, "cleaned up temporary files");
                tasks_run.push(format!("cleanup_temp_files: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to cleanup temp files"),
        }

        // Cleanup expired sessions
        match self.tasks.cleanup_expired_sessions().await {
            Ok(count) => {
                info!(deleted = count, "cleaned up expired sessions");
                tasks_run.push(format!("cleanup_expired_sessions: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to cleanup sessions"),
        }

        // Cleanup form state cache
        match self.tasks.cleanup_form_state_cache().await {
            Ok(count) => {
                info!(deleted = count, "cleaned up form state cache");
                tasks_run.push(format!("cleanup_form_state_cache: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to cleanup form state"),
        }

        // Process queues
        match self.tasks.process_queues().await {
            Ok(count) => {
                info!(processed = count, "processed queue items");
                tasks_run.push(format!("process_queues: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to process queues"),
        }

        // Cleanup expired verification tokens
        match self.tasks.cleanup_verification_tokens().await {
            Ok(count) if count > 0 => {
                info!(count = count, "cleaned up expired verification tokens");
                tasks_run.push(format!("cleanup_verification_tokens: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to cleanup verification tokens"),
            _ => {}
        }

        // Cleanup expired password reset tokens
        match self.tasks.cleanup_password_reset_tokens().await {
            Ok(count) if count > 0 => {
                info!(count = count, "cleaned up expired password reset tokens");
                tasks_run.push(format!("cleanup_password_reset_tokens: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to cleanup password reset tokens"),
            _ => {}
        }

        // Cleanup expired content locks
        match self.tasks.cleanup_expired_locks().await {
            Ok(count) if count > 0 => {
                info!(count = count, "cleaned up expired locks");
                tasks_run.push(format!("cleanup_expired_locks: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to cleanup locks"),
            _ => {}
        }

        // Cleanup audit log (periodic)
        match self.tasks.cleanup_audit_log().await {
            Ok(count) if count > 0 => {
                info!(count = count, "cleaned up old audit log entries");
                tasks_run.push(format!("cleanup_audit_log: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to cleanup audit log"),
            _ => {}
        }

        // Prune the kernel-internal security audit stream (one bounded policy)
        match self.tasks.cleanup_security_audit_log().await {
            Ok(count) if count > 0 => {
                info!(count = count, "pruned old security audit events");
                tasks_run.push(format!("cleanup_security_audit_log: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to prune security audit log"),
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
                    crate::tap::RequestServices::for_background(
                        self.pool.clone(),
                        self.ai_providers.clone(),
                        self.ai_budgets.clone(),
                        self.http.clone(),
                    )
                    .with_plugin_runtime(dispatcher.runtime().clone()),
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
        // double-draining; otherwise the external-cron trigger drives the drain
        // (the default cadence contract, unchanged).
        //
        // The kernel-internal embed queue (P11f) is drained on the same cadence
        // and follows the same resident-runner hand-off, but is independent of
        // any plugin `tap_queue_worker`, so it runs even when no plugin
        // implements one.
        let runner_owns_drain = self.queue_runner_config().await.enabled;
        if runner_owns_drain {
            debug!("resident queue-runner enabled; cron skips queue drains (D-46)");
        } else {
            if let Some(ref dispatcher) = self.tap_dispatcher
                && dispatcher.registry().has_tap("tap_queue_worker")
            {
                match self.drain_plugin_queues().await {
                    Ok(stats) => {
                        tasks_run.push(format!("tap_queue_worker: {}", stats.total()));
                    }
                    Err(e) => warn!(error = %e, "plugin queue dispatch failed"),
                }
            }
            match self.drain_embed_jobs().await {
                Ok(stats) => {
                    if stats.total() > 0 || stats.errors > 0 {
                        tasks_run.push(format!("embed_jobs: {}", stats.total()));
                    }
                }
                Err(e) => warn!(error = %e, "embed queue drain failed"),
            }
        }

        // Rebuild Pagefind index if the trovato_search plugin is enabled and requested it
        if self.pagefind_enabled {
            match pagefind::maybe_rebuild_index(&self.pool, &self.pagefind_static_dir).await {
                Ok(true) => tasks_run.push("pagefind_rebuild".to_string()),
                Ok(false) => {}
                Err(e) => warn!(error = %e, "pagefind index rebuild failed"),
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
        let lock_value = format!("{}:{}", hostname(), std::process::id());

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
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

    /// Release the distributed cron lock.
    ///
    /// Only releases if we own the lock (checked via Lua script).
    async fn release_lock(&self, lock_value: &str) -> Result<()> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
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
    /// For each plugin with claimable items, claims batches of up to the
    /// plugin's honored concurrency (D-47: parsed from `tap_queue_info`, clamped
    /// to `QUEUE_CONCURRENCY_CAP`) via `FOR UPDATE SKIP LOCKED`, dispatches
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
    /// Called both from the in-request cron drain ([`Self::run`]) and, when
    /// enabled, from the resident queue-runner ([`Self::run_queue_runner`]); the
    /// `SKIP LOCKED` claims make concurrent drainers safe.
    pub async fn drain_plugin_queues(&self) -> Result<QueueDrainStats> {
        let mut stats = QueueDrainStats::default();

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
            WHERE (status = 'ready' AND next_attempt_at <= $1)
               OR (status = 'claimed' AND locked_until <= $1)
            ORDER BY plugin_name
            "#,
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .context("failed to query plugin_queue")?;

        for plugin_name in &plugins {
            // Skip plugins that don't implement tap_queue_worker.
            if !dispatcher
                .registry()
                .get_handlers("tap_queue_worker")
                .iter()
                .any(|h| &h.plugin.info.name == plugin_name)
            {
                continue;
            }

            let width = self.plugin_concurrency(&dispatcher, plugin_name).await;
            let mut processed: i64 = 0;

            while processed < MAX_QUEUE_ITEMS_PER_CYCLE {
                let batch = width.min((MAX_QUEUE_ITEMS_PER_CYCLE - processed) as usize);
                let claimed = self.claim_batch(plugin_name, batch).await?;
                if claimed.is_empty() {
                    break;
                }
                processed += claimed.len() as i64;

                // Dispatch the claimed chunk in parallel — this is where a
                // plugin's declared concurrency (bounded by the kernel cap)
                // actually executes concurrently. Each job owns its state and
                // does its own delete/retry/dead-letter bookkeeping.
                let mut set = tokio::task::JoinSet::new();
                for job in claimed {
                    let pool = self.pool.clone();
                    let disp = dispatcher.clone();
                    let ai_providers = self.ai_providers.clone();
                    let ai_budgets = self.ai_budgets.clone();
                    let http = self.http.clone();
                    let plugin = plugin_name.clone();
                    set.spawn(async move {
                        run_queue_job(pool, disp, ai_providers, ai_budgets, http, plugin, job).await
                    });
                }

                while let Some(joined) = set.join_next().await {
                    match joined {
                        Ok(Ok(outcome)) => stats.record(outcome),
                        Ok(Err(e)) => {
                            warn!(error = %e, plugin = %plugin_name, "queue job bookkeeping failed");
                            stats.errors += 1;
                        }
                        Err(e) => {
                            warn!(error = %e, plugin = %plugin_name, "queue job task panicked");
                            stats.errors += 1;
                        }
                    }
                }
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

        let mut processed: i64 = 0;
        while processed < MAX_QUEUE_ITEMS_PER_CYCLE {
            let batch =
                KERNEL_EMBED_CONCURRENCY.min((MAX_QUEUE_ITEMS_PER_CYCLE - processed) as usize);
            let claimed = self.claim_batch(KERNEL_EMBED_PLUGIN, batch).await?;
            if claimed.is_empty() {
                break;
            }
            processed += claimed.len() as i64;

            let mut set = tokio::task::JoinSet::new();
            for job in claimed {
                let pool = self.pool.clone();
                let ai = self.ai_providers.clone();
                let vs = self.vector_store.clone();
                set.spawn(async move { run_embed_job(pool, ai, vs, job).await });
            }

            while let Some(joined) = set.join_next().await {
                match joined {
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

    /// Atomically claim up to `limit` eligible items for `plugin_name`.
    ///
    /// Uses `FOR UPDATE SKIP LOCKED` so concurrent drainers (the cron drain and
    /// the resident runner, or multiple server instances) never claim the same
    /// row. Claiming sets `status = 'claimed'`, extends the lease
    /// (`locked_until`), and increments `attempts` — so a crashed claimer's
    /// attempt still counts toward `max_attempts` (bounded retries,
    /// at-least-once delivery).
    async fn claim_batch(&self, plugin_name: &str, limit: usize) -> Result<Vec<ClaimedJob>> {
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

    /// Resolve the honored worker concurrency for `plugin_name` (D-47).
    ///
    /// Dispatches the plugin's `tap_queue_info` — parsed here for the first time
    /// in the kernel's history; historically `concurrency` was only ever a
    /// codegen docstring — reads the maximum `concurrency` it declares across
    /// its queues, and clamps the result to `[1, QUEUE_CONCURRENCY_CAP]`. Read
    /// fresh each cycle; a plugin that declares nothing (or exports no
    /// `tap_queue_info`) drains at concurrency 1.
    async fn plugin_concurrency(
        &self,
        dispatcher: &Arc<TapDispatcher>,
        plugin_name: &str,
    ) -> usize {
        if !dispatcher
            .registry()
            .get_handlers("tap_queue_info")
            .iter()
            .any(|h| h.plugin.info.name == plugin_name)
        {
            return 1;
        }
        let state = self.background_state(dispatcher);
        let declared = match dispatcher
            .dispatch_to_plugin("tap_queue_info", "{}", plugin_name, state)
            .await
        {
            Some(result) => parse_max_concurrency(&result.output),
            None => 1,
        };
        declared.clamp(1, QUEUE_CONCURRENCY_CAP)
    }

    /// Build a background `RequestState` for kernel-internal tap dispatch.
    ///
    /// Carries the P11c / D-40 background principal so a plugin holding the
    /// `ai_background` capability may call AI from `tap_queue_worker`; the marker
    /// never reaches a web/session context.
    fn background_state(&self, dispatcher: &Arc<TapDispatcher>) -> RequestState {
        RequestState::new(
            crate::tap::UserContext::background(),
            crate::tap::RequestServices::for_background(
                self.pool.clone(),
                self.ai_providers.clone(),
                self.ai_budgets.clone(),
                self.http.clone(),
            )
            .with_plugin_runtime(dispatcher.runtime().clone()),
        )
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

    /// Get the last cron run status.
    pub async fn last_run(&self) -> Result<Option<LastCronRun>> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let data: Option<String> = conn
            .get("cron:last_run")
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
        let run = LastCronRun {
            timestamp: chrono::Utc::now().timestamp(),
            hostname: hostname(),
            result: format!("{result:?}"),
        };

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let json = serde_json::to_string(&run).context("failed to serialize run")?;
        conn.set_ex::<_, _, ()>("cron:last_run", &json, 86400)
            .await
            .context("failed to record run")?;

        Ok(())
    }
}

/// Last cron run information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LastCronRun {
    pub timestamp: i64,
    pub hostname: String,
    pub result: String,
}

/// Run the heartbeat task to extend lock TTL.
async fn run_heartbeat(redis: RedisClient, lock_value: &str, mut stop_rx: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Ok(mut conn) = redis.get_multiplexed_async_connection().await {
                    // Extend lock TTL if we still own it
                    let script = redis::Script::new(EXTEND_LOCK_SCRIPT);
                    if let Err(e) = script
                        .key(CRON_LOCK_KEY)
                        .arg(lock_value)
                        .arg(LOCK_TTL_SECS)
                        .invoke_async::<()>(&mut conn)
                        .await
                    {
                        warn!(error = %e, "failed to extend lock TTL");
                    } else {
                        debug!("extended cron lock TTL");
                    }
                }
            }
            _ = stop_rx.changed() => {
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
        };

        let json = serde_json::to_string(&run).unwrap();
        let parsed: LastCronRun = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.hostname, "test-host");
    }

    // ── P11d: queue v2 pure-logic units (D-47 concurrency, backoff) ──────────

    #[test]
    fn parse_max_concurrency_reads_declared_max() {
        // Ritrovo-style single-queue declaration.
        assert_eq!(
            parse_max_concurrency(r#"[{"name":"ritrovo_import","concurrency":4}]"#),
            4
        );
        // Multiple queues → the maximum wins.
        assert_eq!(
            parse_max_concurrency(r#"[{"name":"a","concurrency":2},{"name":"b","concurrency":7}]"#),
            7
        );
    }

    #[test]
    fn parse_max_concurrency_defaults_to_one() {
        // Empty declaration, missing field, non-array, and garbage all → 1.
        assert_eq!(parse_max_concurrency("[]"), 1);
        assert_eq!(parse_max_concurrency(r#"[{"name":"a"}]"#), 1);
        assert_eq!(parse_max_concurrency(r#"{"not":"an array"}"#), 1);
        assert_eq!(parse_max_concurrency("not json at all"), 1);
        // Zero is not a valid width.
        assert_eq!(
            parse_max_concurrency(r#"[{"name":"a","concurrency":0}]"#),
            1
        );
    }

    #[test]
    fn concurrency_is_clamped_to_the_kernel_cap() {
        // D-47: a plugin may declare any concurrency; the drain clamps it to the
        // kernel cap. This is the "clamping is enforced" contract.
        assert_eq!(QUEUE_CONCURRENCY_CAP, 4);
        let declared = parse_max_concurrency(r#"[{"name":"q","concurrency":64}]"#);
        assert_eq!(declared, 64);
        assert_eq!(declared.clamp(1, QUEUE_CONCURRENCY_CAP), 4);
        // A declaration below the cap is honored, not raised.
        assert_eq!(2usize.clamp(1, QUEUE_CONCURRENCY_CAP), 2);
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
}
