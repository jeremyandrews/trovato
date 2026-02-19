//! Individual cron tasks.

use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::{debug, info};

use super::queue::RedisQueue;
use crate::file::FileService;
use crate::services;

/// Temporary file max age in seconds (6 hours).
const TEMP_FILE_MAX_AGE_SECS: i64 = 6 * 60 * 60;

/// Collection of cron tasks.
pub struct CronTasks {
    pool: PgPool,
    queue: Arc<RedisQueue>,
    files: Option<Arc<FileService>>,
    content_lock: Option<Arc<services::content_lock::ContentLockService>>,
    audit: Option<Arc<services::audit::AuditService>>,
}

impl CronTasks {
    /// Create a new cron tasks instance.
    pub fn new(pool: PgPool, queue: Arc<RedisQueue>) -> Self {
        Self {
            pool,
            queue,
            files: None,
            content_lock: None,
            audit: None,
        }
    }

    /// Create a new cron tasks instance with file service.
    pub fn with_file_service(
        pool: PgPool,
        queue: Arc<RedisQueue>,
        files: Arc<FileService>,
    ) -> Self {
        Self {
            pool,
            queue,
            files: Some(files),
            content_lock: None,
            audit: None,
        }
    }

    /// Set optional plugin services for cron.
    pub fn set_plugin_services(
        &mut self,
        content_lock: Option<Arc<services::content_lock::ContentLockService>>,
        audit: Option<Arc<services::audit::AuditService>>,
    ) {
        self.content_lock = content_lock;
        self.audit = audit;
    }

    /// Cleanup temporary files older than 6 hours.
    ///
    /// Temporary files (status=0) are uploaded but not yet attached
    /// to any content item. After 6 hours, they're considered abandoned.
    /// Deletes both storage files and database records.
    pub async fn cleanup_temp_files(&self) -> Result<u64> {
        // Use FileService if available (deletes both storage and DB)
        if let Some(files) = &self.files {
            return files.cleanup_temp_files(TEMP_FILE_MAX_AGE_SECS).await;
        }

        // Fallback: database-only cleanup (for backwards compatibility)
        let cutoff = chrono::Utc::now().timestamp() - TEMP_FILE_MAX_AGE_SECS;

        let result = sqlx::query(
            r#"
            DELETE FROM file_managed
            WHERE status = 0 AND created < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await;

        match result {
            Ok(res) => Ok(res.rows_affected()),
            Err(e) => {
                // Table might not exist yet
                if e.to_string().contains("file_managed") {
                    debug!("file_managed table not found, skipping cleanup");
                    Ok(0)
                } else {
                    Err(e).context("failed to cleanup temp files")
                }
            }
        }
    }

    /// Cleanup expired sessions.
    ///
    /// Sessions are stored in Redis with TTL, but we also clean up
    /// any stale session data in the database if present.
    pub async fn cleanup_expired_sessions(&self) -> Result<u64> {
        // Calculate cutoff time (sessions older than 24 hours)
        let cutoff = chrono::Utc::now().timestamp() - (24 * 60 * 60);

        // Check if sessions table exists and clean up if so
        let result = sqlx::query(
            r#"
            DELETE FROM sessions
            WHERE updated < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await;

        match result {
            Ok(res) => Ok(res.rows_affected()),
            Err(e) => {
                // Table might not exist
                if e.to_string().contains("sessions") {
                    debug!("sessions table not found, skipping cleanup");
                    Ok(0)
                } else {
                    Err(e).context("failed to cleanup sessions")
                }
            }
        }
    }

    /// Cleanup form state cache entries older than 6 hours.
    pub async fn cleanup_form_state_cache(&self) -> Result<u64> {
        // Calculate cutoff time (6 hours ago)
        let cutoff = chrono::Utc::now().timestamp() - (6 * 60 * 60);

        let result = sqlx::query(
            r#"
            DELETE FROM form_state_cache
            WHERE updated < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .context("failed to cleanup form state cache")?;

        Ok(result.rows_affected())
    }

    /// Process items from background queues.
    ///
    /// Currently processes:
    /// - email:send - Send queued emails
    /// - search:reindex - Reindex items for search
    pub async fn process_queues(&self) -> Result<u64> {
        use super::Queue;

        let mut total_processed = 0u64;

        // Process email queue (up to 50 items per run)
        for _ in 0..50 {
            match self.queue.pop("email:send", 0).await? {
                Some(item) => {
                    if let Err(e) = self.process_email_item(&item).await {
                        info!(error = %e, "failed to process email queue item");
                    }
                    total_processed += 1;
                }
                None => break,
            }
        }

        // Process search reindex queue (up to 100 items per run)
        for _ in 0..100 {
            match self.queue.pop("search:reindex", 0).await? {
                Some(item) => {
                    if let Err(e) = self.process_reindex_item(&item).await {
                        info!(error = %e, "failed to process reindex queue item");
                    }
                    total_processed += 1;
                }
                None => break,
            }
        }

        Ok(total_processed)
    }

    /// Cleanup expired content locks.
    pub async fn cleanup_expired_locks(&self) -> Result<u64> {
        if let Some(ref service) = self.content_lock {
            service.cleanup_expired().await
        } else {
            Ok(0)
        }
    }

    /// Cleanup old audit log entries (90 day retention).
    pub async fn cleanup_audit_log(&self) -> Result<u64> {
        if let Some(ref service) = self.audit {
            service.cleanup(90).await
        } else {
            Ok(0)
        }
    }

    /// Process a single email queue item.
    async fn process_email_item(&self, item: &str) -> Result<()> {
        // Parse email item JSON
        let _email: serde_json::Value =
            serde_json::from_str(item).context("failed to parse email item")?;

        // TODO: Implement actual email sending (Phase 6B or later)
        debug!("would send email: {}", item);
        Ok(())
    }

    /// Process a single reindex queue item.
    async fn process_reindex_item(&self, item: &str) -> Result<()> {
        // Item is just the UUID of the item to reindex
        let item_id: uuid::Uuid = item.parse().context("invalid item ID")?;

        // Touch the item to trigger search_vector update
        sqlx::query(
            r#"
            UPDATE item
            SET changed = $2
            WHERE id = $1
            "#,
        )
        .bind(item_id)
        .bind(chrono::Utc::now().timestamp())
        .execute(&self.pool)
        .await
        .context("failed to reindex item")?;

        debug!(item_id = %item_id, "reindexed item");
        Ok(())
    }
}

impl std::fmt::Debug for CronTasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronTasks").finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[test]
    fn test_cutoff_calculation() {
        let now = chrono::Utc::now().timestamp();
        let six_hours = 6 * 60 * 60;
        let cutoff = now - six_hours;

        // Cutoff should be in the past
        assert!(cutoff < now);
        // Should be exactly 6 hours ago
        assert_eq!(now - cutoff, six_hours);
    }
}
