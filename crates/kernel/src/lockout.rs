//! Account lockout service using Redis.
//!
//! Tracks failed login attempts and temporarily locks accounts
//! after too many failures.

use anyhow::{Context, Result};
use redis::AsyncCommands;
use redis::Client as RedisClient;

/// Maximum failed attempts before lockout.
const MAX_FAILED_ATTEMPTS: u32 = 5;

/// Lockout duration in seconds (15 minutes).
const LOCKOUT_DURATION_SECS: u64 = 15 * 60;

/// Failed attempt tracking window in seconds (15 minutes).
const ATTEMPT_WINDOW_SECS: u64 = 15 * 60;

/// Account lockout service.
#[derive(Clone)]
pub struct LockoutService {
    redis: RedisClient,
}

impl LockoutService {
    /// Create a new lockout service.
    pub fn new(redis: RedisClient) -> Self {
        Self { redis }
    }

    /// Check if an account is currently locked.
    pub async fn is_locked(&self, username: &str) -> Result<bool> {
        let key = lockout_key(username);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let locked: bool = conn
            .exists(&key)
            .await
            .context("failed to check lockout status")?;

        Ok(locked)
    }

    /// Record a failed login attempt.
    ///
    /// Returns (is_now_locked, attempts_remaining).
    pub async fn record_failed_attempt(&self, username: &str) -> Result<(bool, u32)> {
        let attempts_key = attempts_key(username);
        let lockout_key = lockout_key(username);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        // Increment attempt counter
        let attempts: u32 = conn
            .incr(&attempts_key, 1)
            .await
            .context("failed to increment attempt counter")?;

        // Set TTL on first attempt
        if attempts == 1 {
            conn.expire::<_, ()>(&attempts_key, ATTEMPT_WINDOW_SECS as i64)
                .await
                .context("failed to set attempt expiry")?;
        }

        // Check if we should lock
        if attempts >= MAX_FAILED_ATTEMPTS {
            // Set lockout flag
            conn.set_ex::<_, _, ()>(&lockout_key, "locked", LOCKOUT_DURATION_SECS)
                .await
                .context("failed to set lockout")?;

            // Clear attempt counter (will be reset after lockout)
            conn.del::<_, ()>(&attempts_key)
                .await
                .context("failed to clear attempt counter")?;

            tracing::warn!(username = %username, "account locked due to failed attempts");

            return Ok((true, 0));
        }

        let remaining = MAX_FAILED_ATTEMPTS - attempts;
        Ok((false, remaining))
    }

    /// Clear failed attempts after successful login.
    pub async fn clear_attempts(&self, username: &str) -> Result<()> {
        let attempts_key = attempts_key(username);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        conn.del::<_, ()>(&attempts_key)
            .await
            .context("failed to clear attempt counter")?;

        Ok(())
    }

    /// Clear all lockout state (both attempts and lock) for a user.
    pub async fn clear_all(&self, username: &str) -> Result<()> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        conn.del::<_, ()>(&attempts_key(username))
            .await
            .context("failed to clear attempt counter")?;
        conn.del::<_, ()>(&lockout_key(username))
            .await
            .context("failed to clear lockout flag")?;

        Ok(())
    }

    /// Get remaining lockout time in seconds.
    pub async fn get_lockout_remaining(&self, username: &str) -> Result<Option<u64>> {
        let key = lockout_key(username);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let ttl: i64 = conn.ttl(&key).await.context("failed to get lockout TTL")?;

        if ttl > 0 {
            Ok(Some(ttl as u64))
        } else {
            Ok(None)
        }
    }
}

fn attempts_key(username: &str) -> String {
    format!("lockout:attempts:{username}")
}

fn lockout_key(username: &str) -> String {
    format!("lockout:locked:{username}")
}
