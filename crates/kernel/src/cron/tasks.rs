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
    email: Option<Arc<services::email::EmailService>>,
    /// Retention window for the kernel security audit stream, in days.
    ///
    /// Defaults to [`crate::audit::DEFAULT_RETENTION_DAYS`] so a harness that
    /// builds tasks without a `Config` still prunes to a bounded window;
    /// `AppState` overwrites it from [`crate::config::RuntimeConfig`].
    security_audit_retention_days: i64,
    /// How the update check reaches the release endpoint, and whether it may.
    ///
    /// `None` disables the check outright, which is what `UPDATE_CHECK=0` produces
    /// and what a harness that never wants an outbound request gets by default.
    update_check: Option<UpdateCheckConfig>,
}

/// What the update check needs: where to ask, how to ask, and how often.
///
/// Carried on the tasks rather than read from the environment, so a test drives
/// the same code path against a local endpoint and the production path is not a
/// second implementation. `AppState` builds this from
/// [`crate::config::RuntimeConfig`].
#[derive(Clone)]
pub struct UpdateCheckConfig {
    /// The latest-release endpoint.
    pub endpoint: String,
    /// The client to use. Production passes the SSRF-hardened outbound client.
    pub client: reqwest::Client,
    /// Minimum seconds between checks.
    pub interval_secs: u64,
}

impl std::fmt::Debug for UpdateCheckConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateCheckConfig")
            .field("endpoint", &self.endpoint)
            .field("interval_secs", &self.interval_secs)
            .finish_non_exhaustive()
    }
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
            email: None,
            security_audit_retention_days: crate::audit::DEFAULT_RETENTION_DAYS,
            update_check: None,
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
            email: None,
            security_audit_retention_days: crate::audit::DEFAULT_RETENTION_DAYS,
            update_check: None,
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

    /// Set the email service for sending queued emails.
    pub fn set_email_service(&mut self, email: Option<Arc<services::email::EmailService>>) {
        self.email = email;
    }

    /// Set the security-audit retention window, in days.
    ///
    /// Non-positive values are ignored rather than honoured, for the reason given
    /// on `audit::retention_days_from`: a zero-day window prunes the
    /// whole stream.
    /// Configure the update check, or disable it with `None`.
    pub fn set_update_check(&mut self, update_check: Option<UpdateCheckConfig>) {
        self.update_check = update_check;
    }

    /// Ask GitHub whether a newer release exists, at most once per interval.
    ///
    /// Returns `Ok(None)` when nothing was asked: the check is disabled, the site
    /// setting turns it off, or the interval has not elapsed. Every failure past
    /// that point is the caller's to log at debug and ignore. A site whose update
    /// check cannot reach GitHub has a site that works and an administrator who is
    /// not told about releases, and the second is not worth making the first worse.
    ///
    /// # Errors
    ///
    /// Returns the request or storage error, for the caller to log. Nothing
    /// upstream of a page render ever sees it.
    pub async fn check_for_updates(&self) -> Result<Option<crate::update_status::UpdateStatus>> {
        let Some(config) = self.update_check.as_ref() else {
            return Ok(None);
        };

        if !crate::update_status::setting_allows_check(&self.pool).await {
            debug!("update check is turned off for this site");
            return Ok(None);
        }

        let now = chrono::Utc::now().timestamp();
        let last = crate::update_status::stored_status(&self.pool)
            .await
            .map(|status| status.checked_at);
        if !crate::update_status::is_due(last, now, config.interval_secs) {
            return Ok(None);
        }

        let status = crate::update_status::fetch_and_store(
            &self.pool,
            &config.client,
            &config.endpoint,
            now,
        )
        .await?;

        if let Some(status) = status.as_ref() {
            debug!(
                latest = %status.latest_version,
                is_security = status.is_security,
                behind = status.is_behind(),
                "checked for updates"
            );
        }

        Ok(status)
    }

    pub fn set_security_audit_retention_days(&mut self, days: i64) {
        if days > 0 {
            self.security_audit_retention_days = days;
        }
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

    /// Cleanup expired email verification tokens.
    pub async fn cleanup_verification_tokens(&self) -> Result<u64> {
        crate::models::email_verification::EmailVerificationToken::cleanup_expired(&self.pool).await
    }

    /// Cleanup expired password reset tokens.
    pub async fn cleanup_password_reset_tokens(&self) -> Result<u64> {
        crate::models::password_reset::PasswordResetToken::cleanup_expired(&self.pool).await
    }

    /// Cleanup old audit log entries (90 day retention).
    pub async fn cleanup_audit_log(&self) -> Result<u64> {
        if let Some(ref service) = self.audit {
            service.cleanup(90).await
        } else {
            Ok(0)
        }
    }

    /// Prune the kernel-internal security audit stream past its retention window.
    ///
    /// One bounded policy for the whole stream (see [`crate::audit`]): there is
    /// deliberately no per-event-kind retention, so nobody has to reason about
    /// which security events survived a prune. Unlike `cleanup_audit_log` this
    /// is unconditional — the stream is kernel infrastructure, not a plugin
    /// feature, so it always exists and always needs bounding.
    /// The window comes from the `security_audit_retention_days` field, resolved
    /// once at startup — this used to read `SECURITY_AUDIT_RETENTION_DAYS` on
    /// every prune.
    pub async fn cleanup_security_audit_log(&self) -> Result<u64> {
        crate::audit::SecurityAudit::new(self.pool.clone())
            .prune(self.security_audit_retention_days)
            .await
    }

    /// Process a single email queue item.
    ///
    /// Expects JSON with `to`, `subject`, and `body` fields.
    /// Drops the email with a debug log if no email service is configured.
    async fn process_email_item(&self, item: &str) -> Result<()> {
        let email_data: serde_json::Value =
            serde_json::from_str(item).context("failed to parse email item")?;

        let to = email_data
            .get("to")
            .and_then(|v| v.as_str())
            .context("email item missing 'to' field")?;
        let subject = email_data
            .get("subject")
            .and_then(|v| v.as_str())
            .context("email item missing 'subject' field")?;
        let body = email_data
            .get("body")
            .and_then(|v| v.as_str())
            .context("email item missing 'body' field")?;

        let Some(ref email_service) = self.email else {
            debug!(to = %to, subject = %subject, "email service not configured, dropping queued email");
            return Ok(());
        };

        email_service.send(to, subject, body).await?;
        info!(to = %to, subject = %subject, "sent queued email");
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
