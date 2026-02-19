//! Scheduled operations and background tasks.
//!
//! Provides distributed cron with Redis-based locking to ensure
//! exactly-once execution across multiple server instances.

mod queue;
mod tasks;

pub use queue::{Queue, RedisQueue};
pub use tasks::CronTasks;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use redis::{AsyncCommands, Client as RedisClient};
use sqlx::PgPool;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::file::FileService;

/// Lock TTL in seconds (5 minutes).
const LOCK_TTL_SECS: u64 = 300;

/// Heartbeat interval in seconds (60 seconds).
const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// Cron lock key in Redis.
const CRON_LOCK_KEY: &str = "cron:lock";

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

/// Cron service for scheduled operations.
pub struct CronService {
    redis: RedisClient,
    pool: PgPool,
    tasks: CronTasks,
    queue: Arc<RedisQueue>,
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
        }
    }

    /// Set optional plugin services for cron tasks.
    pub fn set_plugin_services(
        &mut self,
        scheduled_publishing: Option<
            std::sync::Arc<crate::services::scheduled_publishing::ScheduledPublishingService>,
        >,
        content_lock: Option<std::sync::Arc<crate::services::content_lock::ContentLockService>>,
        webhooks: Option<std::sync::Arc<crate::services::webhook::WebhookService>>,
        audit: Option<std::sync::Arc<crate::services::audit::AuditService>>,
    ) {
        self.tasks
            .set_plugin_services(scheduled_publishing, content_lock, webhooks, audit);
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

        // Process scheduled publishing
        match self.tasks.process_scheduled_publishing().await {
            Ok(count) if count > 0 => {
                info!(count = count, "processed scheduled publishing");
                tasks_run.push(format!("scheduled_publishing: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to process scheduled publishing"),
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

        // Process webhook deliveries
        match self.tasks.process_webhook_deliveries().await {
            Ok(count) if count > 0 => {
                info!(count = count, "processed webhook deliveries");
                tasks_run.push(format!("webhook_deliveries: {count}"));
            }
            Err(e) => warn!(error = %e, "failed to process webhooks"),
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

        // Stop heartbeat
        let _ = stop_tx.send(true);
        let _ = heartbeat_handle.await;

        // Release lock
        if let Err(e) = self.release_lock(&lock_value).await {
            warn!(error = %e, "failed to release cron lock");
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(duration_ms = duration_ms, tasks = ?tasks_run, "cron completed");

        CronResult::Completed {
            tasks_run,
            duration_ms,
        }
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
}
