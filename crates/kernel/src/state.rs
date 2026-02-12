//! Application state shared across all handlers.

use std::sync::Arc;

use anyhow::{Context, Result};
use redis::Client as RedisClient;
use sqlx::PgPool;

use crate::config::Config;
use crate::db;

/// Shared application state.
///
/// Wrapped in Arc internally so Clone is cheap.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    /// PostgreSQL connection pool.
    pub db: PgPool,

    /// Redis client for sessions and caching.
    pub redis: RedisClient,
}

impl AppState {
    /// Create new application state with database connections.
    pub async fn new(config: &Config) -> Result<Self> {
        // Create PostgreSQL pool
        let db = db::create_pool(config)
            .await
            .context("failed to create database pool")?;

        // Run migrations
        db::run_migrations(&db)
            .await
            .context("failed to run migrations")?;

        // Create Redis client
        let redis = RedisClient::open(config.redis_url.as_str())
            .context("failed to create Redis client")?;

        // Test Redis connection
        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis")?;

        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .context("Redis PING failed")?;

        Ok(Self {
            inner: Arc::new(AppStateInner { db, redis }),
        })
    }

    /// Get the database pool.
    pub fn db(&self) -> &PgPool {
        &self.inner.db
    }

    /// Get the Redis client.
    pub fn redis(&self) -> &RedisClient {
        &self.inner.redis
    }

    /// Check if PostgreSQL is healthy.
    pub async fn postgres_healthy(&self) -> bool {
        db::check_health(&self.inner.db).await
    }

    /// Check if Redis is healthy.
    pub async fn redis_healthy(&self) -> bool {
        let Ok(mut conn) = self.inner.redis.get_multiplexed_async_connection().await else {
            return false;
        };

        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .is_ok()
    }
}
