//! The shared cache: values in Redis, with tag-based invalidation.
//!
//! There is no in-process tier. Every server reads the same Redis, so a value
//! invalidated by one server is gone for all of them at once.

pub mod generation;
pub mod purge;
pub mod tags;

use crate::redis_conn::BoundedRedis;
use std::sync::Arc;

use redis::AsyncCommands;
use redis::Client as RedisClient;
use tracing::{Instrument, debug, warn};

use crate::metrics::{CacheName, Metrics};
use generation::CacheKind;

/// Default TTL for a cached value (5 minutes).
const DEFAULT_TTL_SECS: u64 = 300;

/// Clear `kinds` on every running server, from a process that runs none: the
/// one invalidation path of the command line. `trovato cache clear` is this
/// and nothing else; every other command that writes behind the servers'
/// backs (`config import`, `plugin ...`, `user ...`, `generate`) ends with it.
///
/// Bumps each kind's generation ([`generation`]), waits for Redis to answer,
/// then waits [`generation::SETTLE`], so that by the time it returns every
/// running server has read the new counters and cleared those caches: the
/// request after the command reads the database, not a copy one poll from
/// being dropped. Clearing [`CacheKind::SiteConfig`] also drops the search
/// settings' copy in the shared tier ([`crate::search::settings::SETTINGS_CACHE_KEY`]): the one value read
/// from `site_config` that is held in Redis rather than in a server's memory,
/// so no generation reaches it and it would otherwise wait out its TTL.
pub async fn clear_everywhere(redis: &RedisClient, kinds: &[CacheKind]) -> anyhow::Result<()> {
    generation::bump_now(redis, kinds).await?;
    if kinds.contains(&CacheKind::SiteConfig) {
        let mut conn = redis.bounded_connection().await?;
        conn.del::<_, ()>(crate::search::settings::SETTINGS_CACHE_KEY)
            .await?;
    }
    tokio::time::sleep(generation::SETTLE).await;
    Ok(())
}

/// The shared cache layer.
#[derive(Clone)]
pub struct CacheLayer {
    inner: Arc<CacheLayerInner>,
}

struct CacheLayerInner {
    /// Redis client.
    redis: RedisClient,

    /// Counts hits and misses (the `layer_l2` cache label).
    metrics: Arc<Metrics>,
}

impl CacheLayer {
    /// Create a new cache layer.
    pub fn new(redis: RedisClient, metrics: Arc<Metrics>) -> Self {
        Self {
            inner: Arc::new(CacheLayerInner { redis, metrics }),
        }
    }

    /// Get a value from cache.
    pub async fn get(&self, key: &str) -> Option<String> {
        let span = cache_span("cache.get", key);
        let found = self.get_inner(key).instrument(span.clone()).await;
        span.record("cache.hit", found.is_some());
        found
    }

    async fn get_inner(&self, key: &str) -> Option<String> {
        let metrics = &self.inner.metrics;
        // A Redis that cannot be reached answers nothing, which is a miss to
        // the caller and is counted as one.
        let mut conn = match self.inner.redis.bounded_connection().await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to get Redis connection for cache");
                metrics.count_cache(CacheName::LayerL2, false);
                return None;
            }
        };

        metrics.cache_lookup(
            CacheName::LayerL2,
            conn.get::<_, Option<String>>(key).await.ok().flatten(),
        )
    }

    /// Set a value in cache with TTL and tags.
    pub async fn set(&self, key: &str, value: &str, ttl_secs: u64, tags: &[&str]) {
        self.set_inner(key, value, ttl_secs, tags)
            .instrument(cache_span("cache.set", key))
            .await;
    }

    async fn set_inner(&self, key: &str, value: &str, ttl_secs: u64, tags: &[&str]) {
        let Ok(mut conn) = self.inner.redis.bounded_connection().await else {
            warn!("failed to get Redis connection for cache set");
            return;
        };

        let ttl = if ttl_secs > 0 {
            ttl_secs
        } else {
            DEFAULT_TTL_SECS
        };

        if let Err(e) = conn.set_ex::<_, _, ()>(key, value, ttl).await {
            warn!(error = %e, key = %key, "failed to set cache value in Redis");
            return;
        }
        self.inner.metrics.count_cache_set(CacheName::LayerL2);

        // Register key with each tag
        for tag in tags {
            let tag_key = format!("tag:{tag}");
            if let Err(e) = conn.sadd::<_, _, ()>(&tag_key, key).await {
                warn!(error = %e, tag = %tag, "failed to register cache key with tag");
            }
        }

        debug!(key = %key, tags = ?tags, ttl = %ttl, "cache set");
    }

    /// Invalidate a single cache key.
    pub async fn invalidate(&self, key: &str) {
        self.invalidate_inner(key)
            .instrument(cache_span("cache.invalidate", key))
            .await;
    }

    async fn invalidate_inner(&self, key: &str) {
        let Ok(mut conn) = self.inner.redis.bounded_connection().await else {
            warn!("failed to get Redis connection for cache invalidate");
            return;
        };

        if let Err(e) = conn.del::<_, ()>(key).await {
            warn!(error = %e, key = %key, "failed to delete cache key from Redis");
        }

        debug!(key = %key, "cache invalidated");
    }

    /// Invalidate all cache keys associated with a tag.
    ///
    /// Uses Lua script for atomic operation.
    pub async fn invalidate_tag(&self, tag: &str) {
        self.invalidate_tag_inner(tag)
            .instrument(cache_span("cache.invalidate_tag", tag))
            .await;
    }

    async fn invalidate_tag_inner(&self, tag: &str) {
        let tag_key = format!("tag:{tag}");

        let Ok(mut conn) = self.inner.redis.bounded_connection().await else {
            warn!("failed to get Redis connection for tag invalidation");
            return;
        };

        // Use Lua script for atomic Redis invalidation
        let script = redis::Script::new(INVALIDATE_TAG_SCRIPT);
        match script.key(&tag_key).invoke_async::<usize>(&mut conn).await {
            Ok(count) => debug!(tag = %tag, keys_invalidated = %count, "tag invalidated"),
            Err(e) => warn!(error = %e, tag = %tag, "failed to invalidate tag in Redis"),
        }
    }

    /// Generate a stage-scoped cache key.
    ///
    /// Live stage uses bare keys for maximum cache hit rates.
    /// Non-live stages use prefixed keys to isolate preview data.
    pub fn stage_key(key: &str, stage_id: Option<uuid::Uuid>) -> String {
        match stage_id {
            None => key.to_string(),
            Some(st) if st == crate::models::stage::LIVE_STAGE_ID => key.to_string(),
            Some(st) => format!("st:{st}:{key}"),
        }
    }

    /// Generate a tenant-and-stage-scoped cache key.
    ///
    /// Default tenant uses the same format as `stage_key` for backward
    /// compatibility (no tenant prefix). Non-default tenants get
    /// `t:{tenant_id}:st:{stage_id}:{key}` or `t:{tenant_id}:{key}` for live stage.
    pub fn tenant_stage_key(
        key: &str,
        tenant_id: Option<uuid::Uuid>,
        stage_id: Option<uuid::Uuid>,
    ) -> String {
        let stage_part = Self::stage_key(key, stage_id);

        match tenant_id {
            None => stage_part,
            Some(t) if t == crate::models::tenant::DEFAULT_TENANT_ID => stage_part,
            Some(t) => format!("t:{t}:{stage_part}"),
        }
    }

    /// Invalidate all cache keys for a stage.
    ///
    /// Used when publishing a stage to live.
    pub async fn invalidate_stage(&self, stage_id: uuid::Uuid) {
        if stage_id == crate::models::stage::LIVE_STAGE_ID {
            warn!("attempted to invalidate live stage cache - ignoring");
            return;
        }

        let pattern = format!("st:{stage_id}:*");

        let Ok(mut conn) = self.inner.redis.bounded_connection().await else {
            warn!("failed to get Redis connection for stage invalidation");
            return;
        };

        // Use SCAN to find and delete all matching keys
        let mut cursor = 0u64;
        let mut total_deleted = 0usize;

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = match redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    warn!(error = %e, "SCAN failed during stage invalidation");
                    break;
                }
            };

            if !keys.is_empty() {
                // Delete from Redis
                if let Err(e) = conn.del::<_, ()>(&keys).await {
                    warn!(error = %e, "failed to delete stage keys from Redis");
                }

                total_deleted += keys.len();
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        debug!(stage_id = %stage_id, keys_deleted = %total_deleted, "stage cache invalidated");
    }

    /// Get cache statistics (for monitoring), read from Redis.
    ///
    /// `redis_keys` is `None` when Redis cannot be reached.
    pub async fn stats(&self) -> CacheStats {
        let redis_keys = match self.inner.redis.bounded_connection().await {
            Ok(mut conn) => redis::cmd("DBSIZE")
                .query_async::<u64>(&mut conn)
                .await
                .ok(),
            Err(_) => None,
        };
        CacheStats { redis_keys }
    }
}

/// The span of one Redis cache call: its operation and the key's bin, never
/// the key itself.
fn cache_span(operation: &'static str, key: &str) -> tracing::Span {
    tracing::trace_span!(
        target: crate::telemetry::TARGET,
        "cache",
        otel.name = operation,
        cache.tier = "redis",
        cache.bin = crate::telemetry::cache_bin(key),
        cache.hit = tracing::field::Empty,
    )
}

/// Cache statistics.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Number of keys in the Redis database the cache uses (which also holds
    /// sessions, locks and queues), or `None` if Redis was unreachable.
    pub redis_keys: Option<u64>,
}

/// Lua script for atomic tag invalidation.
///
/// Gets all keys in the tag set, deletes them, then deletes the tag set.
const INVALIDATE_TAG_SCRIPT: &str = r#"
local keys = redis.call("SMEMBERS", KEYS[1])
if #keys > 0 then
    redis.call("DEL", unpack(keys))
end
redis.call("DEL", KEYS[1])
return #keys
"#;

impl std::fmt::Debug for CacheLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheLayer").finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_stage_key_live() {
        assert_eq!(CacheLayer::stage_key("item:123", None), "item:123");
        assert_eq!(
            CacheLayer::stage_key("item:123", Some(crate::models::stage::LIVE_STAGE_ID)),
            "item:123"
        );
    }

    #[test]
    fn test_stage_key_non_live() {
        let preview = uuid::Uuid::now_v7();
        assert_eq!(
            CacheLayer::stage_key("item:123", Some(preview)),
            format!("st:{preview}:item:123")
        );
    }

    #[tokio::test]
    async fn test_cache_layer_creation() {
        // This test requires Redis, so we just verify the struct can be created
        // In a real test environment, we'd use a mock or test Redis instance
        let client = RedisClient::open(trovato_test_utils::env::redis_url()).unwrap();
        let cache = CacheLayer::new(client, Default::default());

        // With no Redis the count is simply absent; it never panics.
        let _ = cache.stats().await;
    }
}
