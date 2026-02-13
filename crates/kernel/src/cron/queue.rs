//! Redis-backed queue for background task processing.

use anyhow::{Context, Result};
use async_trait::async_trait;
use redis::{AsyncCommands, Client as RedisClient};
use tracing::debug;

/// Queue trait for background task processing.
#[async_trait]
pub trait Queue: Send + Sync {
    /// Push an item onto the queue.
    async fn push(&self, queue: &str, item: &str) -> Result<()>;

    /// Pop an item from the queue (blocking with timeout).
    async fn pop(&self, queue: &str, timeout_secs: u64) -> Result<Option<String>>;

    /// Get the number of items in the queue.
    async fn len(&self, queue: &str) -> Result<u64>;

    /// Check if the queue is empty.
    async fn is_empty(&self, queue: &str) -> Result<bool> {
        Ok(self.len(queue).await? == 0)
    }
}

/// Redis-backed queue implementation.
pub struct RedisQueue {
    redis: RedisClient,
}

impl RedisQueue {
    /// Create a new Redis queue.
    pub fn new(redis: RedisClient) -> Self {
        Self { redis }
    }

    /// Get the full queue key with prefix.
    fn queue_key(&self, queue: &str) -> String {
        format!("queue:{}", queue)
    }
}

#[async_trait]
impl Queue for RedisQueue {
    async fn push(&self, queue: &str, item: &str) -> Result<()> {
        let key = self.queue_key(queue);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        conn.rpush::<_, _, ()>(&key, item)
            .await
            .context("failed to push to queue")?;

        debug!(queue = %queue, "pushed item to queue");
        Ok(())
    }

    async fn pop(&self, queue: &str, timeout_secs: u64) -> Result<Option<String>> {
        let key = self.queue_key(queue);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        // BLPOP returns (key, value) tuple or nil
        let result: Option<(String, String)> = if timeout_secs > 0 {
            conn.blpop(&key, timeout_secs as f64)
                .await
                .context("failed to pop from queue")?
        } else {
            // Non-blocking pop
            conn.lpop(&key, None)
                .await
                .map(|v: Option<String>| v.map(|s| (key.clone(), s)))
                .context("failed to pop from queue")?
        };

        match result {
            Some((_, value)) => {
                debug!(queue = %queue, "popped item from queue");
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    async fn len(&self, queue: &str) -> Result<u64> {
        let key = self.queue_key(queue);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let len: u64 = conn.llen(&key).await.context("failed to get queue length")?;

        Ok(len)
    }
}

impl std::fmt::Debug for RedisQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisQueue").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_key() {
        let client = RedisClient::open("redis://127.0.0.1:6379").unwrap();
        let queue = RedisQueue::new(client);
        assert_eq!(queue.queue_key("test"), "queue:test");
        assert_eq!(queue.queue_key("email:send"), "queue:email:send");
    }
}
