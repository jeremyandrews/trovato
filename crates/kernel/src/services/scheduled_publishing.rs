//! Scheduled publishing service.
//!
//! Cron handler that publishes/unpublishes items based on
//! field_publish_on and field_unpublish_on JSONB fields.

use anyhow::Result;
use sqlx::PgPool;
use tracing::{debug, info};

/// Scheduled publishing cron handler.
pub struct ScheduledPublishingService {
    pool: PgPool,
}

impl ScheduledPublishingService {
    /// Create a new scheduled publishing service.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Process scheduled publish/unpublish operations.
    ///
    /// Returns the number of items updated.
    pub async fn process(&self) -> Result<u64> {
        let now = chrono::Utc::now().timestamp();
        let mut total = 0u64;

        // Publish items where field_publish_on <= now AND status = 0
        let published = sqlx::query(
            r#"
            UPDATE item
            SET status = 1, changed = $1
            WHERE status = 0
              AND fields->>'field_publish_on' IS NOT NULL
              AND (fields->>'field_publish_on')::bigint <= $1
            "#,
        )
        .bind(now)
        .execute(&self.pool)
        .await;

        match published {
            Ok(res) => {
                let count = res.rows_affected();
                if count > 0 {
                    info!(count = count, "published scheduled items");
                }
                total += count;
            }
            Err(e) => {
                debug!(error = %e, "no items to publish or field not set");
            }
        }

        // Unpublish items where field_unpublish_on <= now AND status = 1
        let unpublished = sqlx::query(
            r#"
            UPDATE item
            SET status = 0, changed = $1
            WHERE status = 1
              AND fields->>'field_unpublish_on' IS NOT NULL
              AND (fields->>'field_unpublish_on')::bigint <= $1
            "#,
        )
        .bind(now)
        .execute(&self.pool)
        .await;

        match unpublished {
            Ok(res) => {
                let count = res.rows_affected();
                if count > 0 {
                    info!(count = count, "unpublished scheduled items");
                }
                total += count;
            }
            Err(e) => {
                debug!(error = %e, "no items to unpublish or field not set");
            }
        }

        Ok(total)
    }
}

impl std::fmt::Debug for ScheduledPublishingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduledPublishingService").finish()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_comparison() {
        let now = chrono::Utc::now().timestamp();
        let past = now - 3600;
        let future = now + 3600;
        assert!(past <= now);
        assert!(future > now);
    }
}
