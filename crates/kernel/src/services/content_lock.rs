//! Content locking service.
//!
//! Pessimistic locking to prevent concurrent editing of the same entity.

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

/// Default lock duration in seconds (15 minutes).
const DEFAULT_LOCK_DURATION_SECS: i64 = 900;

/// Maximum absolute lock lifetime in seconds (24 hours).
/// Prevents indefinite lock extension via repeated heartbeats.
const MAX_ABSOLUTE_LOCK_LIFETIME_SECS: i64 = 86400;

/// Lock information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct EditingLock {
    pub entity_type: String,
    pub entity_id: String,
    pub user_id: Uuid,
    pub locked_at: i64,
    pub expires_at: i64,
}

/// Content locking service.
#[derive(Clone)]
pub struct ContentLockService {
    pool: PgPool,
}

impl ContentLockService {
    /// Create a new content lock service.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Acquire a lock on an entity.
    ///
    /// Returns Ok(true) if lock acquired, Ok(false) if already locked by another user.
    pub async fn acquire(&self, entity_type: &str, entity_id: &str, user_id: Uuid) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let expires_at = now + DEFAULT_LOCK_DURATION_SECS;

        // Try to insert or update if expired or same user
        let result = sqlx::query(
            r#"
            INSERT INTO editing_lock (entity_type, entity_id, user_id, locked_at, expires_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (entity_type, entity_id) DO UPDATE
            SET user_id = $3, locked_at = $4, expires_at = $5
            WHERE editing_lock.user_id = $3 OR editing_lock.expires_at < $4
            "#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(user_id)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .context("failed to acquire lock")?;

        let acquired = result.rows_affected() > 0;
        debug!(
            entity_type = %entity_type,
            entity_id = %entity_id,
            acquired = acquired,
            "lock acquisition attempt"
        );

        Ok(acquired)
    }

    /// Release a lock held by a specific user.
    pub async fn release(&self, entity_type: &str, entity_id: &str, user_id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            r#"
            DELETE FROM editing_lock
            WHERE entity_type = $1 AND entity_id = $2 AND user_id = $3
            "#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .context("failed to release lock")?;

        Ok(result.rows_affected() > 0)
    }

    /// Check if an entity is locked.
    pub async fn check(&self, entity_type: &str, entity_id: &str) -> Result<Option<EditingLock>> {
        let now = chrono::Utc::now().timestamp();

        let lock = sqlx::query_as::<_, EditingLock>(
            r#"
            SELECT entity_type, entity_id, user_id, locked_at, expires_at
            FROM editing_lock
            WHERE entity_type = $1 AND entity_id = $2 AND expires_at > $3
            "#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .context("failed to check lock")?;

        Ok(lock)
    }

    /// Extend lock expiration (heartbeat).
    ///
    /// Enforces a maximum absolute lifetime from the original lock time to
    /// prevent indefinite lock extension via repeated heartbeats.
    pub async fn heartbeat(
        &self,
        entity_type: &str,
        entity_id: &str,
        user_id: Uuid,
    ) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let expires_at = now + DEFAULT_LOCK_DURATION_SECS;

        // Only extend if the lock hasn't exceeded its maximum absolute lifetime.
        let result = sqlx::query(
            r#"
            UPDATE editing_lock
            SET expires_at = $1
            WHERE entity_type = $2 AND entity_id = $3 AND user_id = $4
              AND locked_at + $5 > $6
            "#,
        )
        .bind(expires_at)
        .bind(entity_type)
        .bind(entity_id)
        .bind(user_id)
        .bind(MAX_ABSOLUTE_LOCK_LIFETIME_SECS)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to heartbeat lock")?;

        Ok(result.rows_affected() > 0)
    }

    /// Break a lock regardless of owner (requires "break content lock" permission).
    pub async fn break_lock(&self, entity_type: &str, entity_id: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            DELETE FROM editing_lock
            WHERE entity_type = $1 AND entity_id = $2
            "#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .execute(&self.pool)
        .await
        .context("failed to break lock")?;

        Ok(result.rows_affected() > 0)
    }

    /// Cleanup expired locks.
    pub async fn cleanup_expired(&self) -> Result<u64> {
        let now = chrono::Utc::now().timestamp();

        let result = sqlx::query("DELETE FROM editing_lock WHERE expires_at < $1")
            .bind(now)
            .execute(&self.pool)
            .await;

        match result {
            Ok(res) => Ok(res.rows_affected()),
            Err(e) => {
                if e.to_string().contains("editing_lock") {
                    debug!("editing_lock table not found, skipping cleanup");
                    Ok(0)
                } else {
                    Err(e).context("failed to cleanup expired locks")
                }
            }
        }
    }
}

impl std::fmt::Debug for ContentLockService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentLockService").finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn lock_duration_is_15_minutes() {
        assert_eq!(DEFAULT_LOCK_DURATION_SECS, 900);
    }

    #[test]
    fn max_absolute_lifetime_is_24_hours() {
        assert_eq!(MAX_ABSOLUTE_LOCK_LIFETIME_SECS, 86400);
        // Static check: max lifetime must be much larger than single lock duration.
        const _: () = assert!(MAX_ABSOLUTE_LOCK_LIFETIME_SECS > DEFAULT_LOCK_DURATION_SECS * 10);
    }

    #[test]
    fn editing_lock_serialization() {
        let lock = EditingLock {
            entity_type: "item".to_string(),
            entity_id: Uuid::nil().to_string(),
            user_id: Uuid::nil(),
            locked_at: 1000,
            expires_at: 1900,
        };
        let json = serde_json::to_string(&lock).unwrap();
        assert!(json.contains("item"));
    }
}
