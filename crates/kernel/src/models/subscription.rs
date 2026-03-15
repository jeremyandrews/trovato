//! User subscription model for item follow/notification tracking.

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

/// A user subscription to an item.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct Subscription {
    /// The subscribing user's ID.
    pub user_id: Uuid,
    /// The subscribed item's ID.
    pub item_id: Uuid,
    /// Unix timestamp when the subscription was created.
    pub created: i64,
}

impl Subscription {
    /// Subscribe a user to an item.
    ///
    /// No-op if already subscribed.
    pub async fn subscribe(pool: &PgPool, user_id: Uuid, item_id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_subscriptions (user_id, item_id) \
             VALUES ($1, $2) \
             ON CONFLICT (user_id, item_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(item_id)
        .execute(pool)
        .await
        .context("failed to subscribe")?;
        Ok(())
    }

    /// Unsubscribe a user from an item.
    ///
    /// Returns `true` if a subscription was removed, `false` if none existed.
    pub async fn unsubscribe(pool: &PgPool, user_id: Uuid, item_id: Uuid) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM user_subscriptions WHERE user_id = $1 AND item_id = $2")
                .bind(user_id)
                .bind(item_id)
                .execute(pool)
                .await
                .context("failed to unsubscribe")?;
        Ok(result.rows_affected() > 0)
    }

    /// Check if a user is subscribed to an item.
    pub async fn is_subscribed(pool: &PgPool, user_id: Uuid, item_id: Uuid) -> Result<bool> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM user_subscriptions WHERE user_id = $1 AND item_id = $2)",
        )
        .bind(user_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .context("failed to check subscription")?;
        Ok(exists)
    }

    /// List all subscriber user IDs for an item, ordered by subscription time.
    pub async fn list_subscribers(pool: &PgPool, item_id: Uuid) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT user_id FROM user_subscriptions WHERE item_id = $1 ORDER BY created",
        )
        .bind(item_id)
        .fetch_all(pool)
        .await
        .context("failed to list subscribers")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// List all subscriptions for a user, most recent first.
    pub async fn list_user_subscriptions(
        pool: &PgPool,
        user_id: Uuid,
    ) -> Result<Vec<Subscription>> {
        let rows = sqlx::query_as::<_, Subscription>(
            "SELECT user_id, item_id, created \
             FROM user_subscriptions \
             WHERE user_id = $1 \
             ORDER BY created DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .context("failed to list user subscriptions")?;
        Ok(rows)
    }
}
