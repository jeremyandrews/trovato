//! Rate limiting middleware using Redis for distributed counting.
//!
//! Uses a sliding window counter pattern with Redis INCR + EXPIRE.

use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use redis::AsyncCommands;
use redis::Client as RedisClient;
use tracing::{debug, warn};

/// Rate limit configuration for different endpoint categories.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Login attempts: (max requests, window duration)
    pub login: (u32, Duration),
    /// Form submissions
    pub forms: (u32, Duration),
    /// API endpoints
    pub api: (u32, Duration),
    /// Search queries
    pub search: (u32, Duration),
    /// File uploads
    pub uploads: (u32, Duration),
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            login: (5, Duration::from_secs(60)),    // 5 per minute
            forms: (30, Duration::from_secs(60)),   // 30 per minute
            api: (100, Duration::from_secs(60)),    // 100 per minute
            search: (20, Duration::from_secs(60)),  // 20 per minute
            uploads: (10, Duration::from_secs(60)), // 10 per minute
        }
    }
}

/// Rate limiter using Redis for distributed counting.
#[derive(Clone)]
pub struct RateLimiter {
    redis: RedisClient,
    config: RateLimitConfig,
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new(redis: RedisClient, config: RateLimitConfig) -> Self {
        Self { redis, config }
    }

    /// Check if a request should be rate limited.
    ///
    /// Returns Ok(()) if allowed, Err with retry-after seconds if limited.
    pub async fn check(&self, category: &str, identifier: &str) -> Result<(), u64> {
        let (limit, window) = self.get_limit(category);
        let key = format!("rate:{category}:{identifier}");
        let window_secs = window.as_secs();

        let count = match self.increment(&key, window_secs).await {
            Ok(c) => c,
            Err(e) => {
                // If Redis fails, allow the request (fail open)
                warn!(error = %e, "rate limit check failed, allowing request");
                return Ok(());
            }
        };

        if count > limit as i64 {
            debug!(
                category = category,
                identifier = identifier,
                count = count,
                limit = limit,
                "rate limit exceeded"
            );
            Err(window_secs)
        } else {
            Ok(())
        }
    }

    /// Get the rate limit for a category.
    fn get_limit(&self, category: &str) -> (u32, Duration) {
        match category {
            "login" => self.config.login,
            "forms" => self.config.forms,
            "api" => self.config.api,
            "search" => self.config.search,
            "uploads" => self.config.uploads,
            _ => self.config.api, // Default to API limits
        }
    }

    /// Increment the counter and return the new value.
    async fn increment(&self, key: &str, ttl_secs: u64) -> Result<i64, redis::RedisError> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;

        // INCR and set TTL if new key
        let count: i64 = conn.incr(key, 1).await?;

        if count == 1 {
            // Set expiry on first increment
            let _: () = conn.expire(key, ttl_secs as i64).await?;
        }

        Ok(count)
    }

    /// Get the current count for a key (for monitoring).
    pub async fn get_count(
        &self,
        category: &str,
        identifier: &str,
    ) -> Result<i64, redis::RedisError> {
        let key = format!("rate:{category}:{identifier}");
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let count: Option<i64> = conn.get(&key).await?;
        Ok(count.unwrap_or(0))
    }

    /// Reset the counter for a key (for testing).
    pub async fn reset(&self, category: &str, identifier: &str) -> Result<(), redis::RedisError> {
        let key = format!("rate:{category}:{identifier}");
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let _: () = conn.del(&key).await?;
        Ok(())
    }
}

/// Categorize a request path for rate limiting.
pub fn categorize_path(path: &str, method: &str) -> &'static str {
    if path.starts_with("/user/login") && method == "POST" {
        "login"
    } else if path.starts_with("/file/upload") {
        "uploads"
    } else if path.starts_with("/search") || path.starts_with("/api/search") {
        "search"
    } else if path.starts_with("/api/") {
        "api"
    } else if method == "POST" {
        "forms"
    } else {
        "api" // Default category for GET requests
    }
}

/// Get the client identifier (IP address) for rate limiting.
pub fn get_client_id(
    addr: Option<std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
) -> String {
    // Check X-Forwarded-For header first (for proxied requests)
    if let Some(forwarded) = headers.get("x-forwarded-for")
        && let Ok(value) = forwarded.to_str()
    {
        // Take the first IP in the chain
        if let Some(ip) = value.split(',').next() {
            return ip.trim().to_string();
        }
    }

    // Check X-Real-IP header
    if let Some(real_ip) = headers.get("x-real-ip")
        && let Ok(value) = real_ip.to_str()
    {
        return value.to_string();
    }

    // Fall back to connection address
    addr.map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Rate limit exceeded response.
pub fn rate_limit_response(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            ("retry-after", retry_after.to_string()),
            ("content-type", "application/json".to_string()),
        ],
        format!(r#"{{"error":"Rate limit exceeded","retry_after":{retry_after}}}"#),
    )
        .into_response()
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("config", &self.config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorize_path() {
        assert_eq!(categorize_path("/user/login", "POST"), "login");
        assert_eq!(categorize_path("/user/login/json", "POST"), "login");
        assert_eq!(categorize_path("/file/upload", "POST"), "uploads");
        assert_eq!(categorize_path("/search", "GET"), "search");
        assert_eq!(categorize_path("/api/search", "GET"), "search");
        assert_eq!(categorize_path("/api/items", "GET"), "api");
        assert_eq!(categorize_path("/item/123", "POST"), "forms");
    }

    #[test]
    fn test_default_config() {
        let config = RateLimitConfig::default();
        assert_eq!(config.login.0, 5);
        assert_eq!(config.api.0, 100);
    }
}
