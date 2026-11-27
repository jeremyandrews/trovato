//! Redis-backed queue for background task processing.

use crate::redis_conn::BoundedRedis;
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
    /// Prepended to every key, so `email:send` lives at `{prefix}email:send`.
    prefix: String,
}

impl RedisQueue {
    /// Create a new Redis queue.
    pub fn new(redis: RedisClient) -> Self {
        Self::with_key_prefix(redis, "queue:")
    }

    /// A queue whose keys start with `prefix` instead of `queue:`.
    ///
    /// For a test that drives the real queue names (`process_queues` pops
    /// `email:send` and `search:reindex` by name) without touching the site's
    /// own queues on a shared Redis: it gives each run keys of its own.
    pub fn with_key_prefix(redis: RedisClient, prefix: impl Into<String>) -> Self {
        Self {
            redis,
            prefix: prefix.into(),
        }
    }

    /// Get the full queue key with prefix.
    fn queue_key(&self, queue: &str) -> String {
        format!("{}{queue}", self.prefix)
    }
}

#[async_trait]
impl Queue for RedisQueue {
    async fn push(&self, queue: &str, item: &str) -> Result<()> {
        let key = self.queue_key(queue);

        let mut conn = self
            .redis
            .bounded_connection()
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

        // A blocking pop legitimately waits `timeout_secs` for its reply, so
        // its connection's response timeout is that plus a margin; a plain pop
        // gets the ordinary one. Either way a Redis that has stopped answering
        // turns into an error here instead of an await that never returns.
        let connection = if timeout_secs > 0 {
            self.redis
                .bounded_blocking_connection(std::time::Duration::from_secs(timeout_secs))
                .await
        } else {
            self.redis.bounded_connection().await
        };
        let mut conn = connection.context("failed to get Redis connection")?;

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
            .bounded_connection()
            .await
            .context("failed to get Redis connection")?;

        let len: u64 = conn
            .llen(&key)
            .await
            .context("failed to get queue length")?;

        Ok(len)
    }
}

impl std::fmt::Debug for RedisQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisQueue").finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_key() {
        let client = RedisClient::open(trovato_test_utils::env::redis_url()).unwrap();
        let queue = RedisQueue::new(client);
        assert_eq!(queue.queue_key("test"), "queue:test");
        assert_eq!(queue.queue_key("email:send"), "queue:email:send");
    }

    /// A prefixed queue keeps every key under its prefix, so a test that
    /// drives `email:send` this way never reads or deletes the real one.
    #[test]
    fn a_prefixed_queue_keeps_its_keys_to_itself() {
        let client = RedisClient::open(trovato_test_utils::env::redis_url()).unwrap();
        let queue = RedisQueue::with_key_prefix(client, "test:run-1:");
        assert_eq!(queue.queue_key("email:send"), "test:run-1:email:send");
        assert!(!queue.queue_key("search:reindex").starts_with("queue:"));
    }
}
