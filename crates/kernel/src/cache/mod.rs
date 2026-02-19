//! Two-tier cache with Moka (L1) and Redis (L2).
//!
//! Supports tag-based invalidation for efficient cache management.

use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use redis::AsyncCommands;
use redis::Client as RedisClient;
use tracing::{debug, warn};

/// Default TTL for L1 cache (60 seconds).
const L1_TTL_SECS: u64 = 60;

/// Default TTL for L2 cache (5 minutes).
const L2_TTL_SECS: u64 = 300;

/// Maximum L1 cache capacity.
const L1_MAX_CAPACITY: u64 = 10_000;

/// Two-tier cache layer.
///
/// L1 (Moka): In-process, short TTL, per-instance
/// L2 (Redis): Shared across instances, longer TTL
#[derive(Clone)]
pub struct CacheLayer {
    inner: Arc<CacheLayerInner>,
}

struct CacheLayerInner {
    /// L1 in-process cache.
    local: Cache<String, String>,

    /// L2 Redis client.
    redis: RedisClient,
}

impl CacheLayer {
    /// Create a new cache layer.
    pub fn new(redis: RedisClient) -> Self {
        let local = Cache::builder()
            .max_capacity(L1_MAX_CAPACITY)
            .time_to_live(Duration::from_secs(L1_TTL_SECS))
            .build();

        Self {
            inner: Arc::new(CacheLayerInner { local, redis }),
        }
    }

    /// Get a value from cache.
    ///
    /// Checks L1 first, then L2. On L2 hit, populates L1.
    pub async fn get(&self, key: &str) -> Option<String> {
        // Check L1 first
        if let Some(val) = self.inner.local.get(key).await {
            debug!(key = %key, "cache L1 hit");
            return Some(val);
        }

        // Check L2
        let mut conn = match self.inner.redis.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to get Redis connection for cache");
                return None;
            }
        };

        let val: Option<String> = conn.get(key).await.ok()?;

        if let Some(ref v) = val {
            debug!(key = %key, "cache L2 hit, populating L1");
            self.inner.local.insert(key.to_string(), v.clone()).await;
        }

        val
    }

    /// Set a value in cache with TTL and tags.
    ///
    /// Writes to both L1 and L2.
    pub async fn set(&self, key: &str, value: &str, ttl_secs: u64, tags: &[&str]) {
        // Set in L1
        self.inner
            .local
            .insert(key.to_string(), value.to_string())
            .await;

        // Set in L2 with TTL
        let Ok(mut conn) = self.inner.redis.get_multiplexed_async_connection().await else {
            warn!("failed to get Redis connection for cache set");
            return;
        };

        let ttl = if ttl_secs > 0 { ttl_secs } else { L2_TTL_SECS };

        if let Err(e) = conn.set_ex::<_, _, ()>(key, value, ttl).await {
            warn!(error = %e, key = %key, "failed to set cache value in Redis");
            return;
        }

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
        // Invalidate L1
        self.inner.local.invalidate(key).await;

        // Invalidate L2
        let Ok(mut conn) = self.inner.redis.get_multiplexed_async_connection().await else {
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
        let tag_key = format!("tag:{tag}");

        let Ok(mut conn) = self.inner.redis.get_multiplexed_async_connection().await else {
            warn!("failed to get Redis connection for tag invalidation");
            return;
        };

        // Get all keys for this tag
        let keys: Vec<String> = match conn.smembers(&tag_key).await {
            Ok(k) => k,
            Err(e) => {
                warn!(error = %e, tag = %tag, "failed to get tag members");
                return;
            }
        };

        // Invalidate L1 for all tagged keys
        for key in &keys {
            self.inner.local.invalidate(key).await;
        }

        // Use Lua script for atomic Redis invalidation
        let script = redis::Script::new(INVALIDATE_TAG_SCRIPT);
        if let Err(e) = script.key(&tag_key).invoke_async::<()>(&mut conn).await {
            warn!(error = %e, tag = %tag, "failed to invalidate tag in Redis");
            return;
        }

        debug!(tag = %tag, keys_invalidated = %keys.len(), "tag invalidated");
    }

    /// Generate a stage-scoped cache key.
    ///
    /// Live stage uses bare keys for maximum cache hit rates.
    /// Non-live stages use prefixed keys to isolate preview data.
    pub fn stage_key(key: &str, stage_id: Option<&str>) -> String {
        match stage_id {
            None | Some("live") => key.to_string(),
            Some(st) => format!("st:{st}:{key}"),
        }
    }

    /// Invalidate all cache keys for a stage.
    ///
    /// Used when publishing a stage to live.
    pub async fn invalidate_stage(&self, stage_id: &str) {
        if stage_id == "live" {
            warn!("attempted to invalidate live stage cache - ignoring");
            return;
        }

        let pattern = format!("st:{stage_id}:*");

        let Ok(mut conn) = self.inner.redis.get_multiplexed_async_connection().await else {
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
                // Invalidate L1
                for key in &keys {
                    self.inner.local.invalidate(key).await;
                }

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

    /// Get cache statistics (for monitoring).
    pub async fn stats(&self) -> CacheStats {
        CacheStats {
            l1_entry_count: self.inner.local.entry_count(),
            l1_weighted_size: self.inner.local.weighted_size(),
        }
    }
}

/// Cache statistics.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Number of entries in L1 cache.
    pub l1_entry_count: u64,

    /// Weighted size of L1 cache.
    pub l1_weighted_size: u64,
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
        assert_eq!(CacheLayer::stage_key("item:123", Some("live")), "item:123");
    }

    #[test]
    fn test_stage_key_non_live() {
        assert_eq!(
            CacheLayer::stage_key("item:123", Some("preview-abc")),
            "st:preview-abc:item:123"
        );
    }

    #[tokio::test]
    async fn test_cache_layer_creation() {
        // This test requires Redis, so we just verify the struct can be created
        // In a real test environment, we'd use a mock or test Redis instance
        let client = RedisClient::open("redis://127.0.0.1:6379").unwrap();
        let cache = CacheLayer::new(client);

        let stats = cache.stats().await;
        assert_eq!(stats.l1_entry_count, 0);
    }
}
