//! Session management using Redis.

use anyhow::{Context, Result};
use fred::prelude::*;
use tower_sessions::cookie::SameSite;
use tower_sessions::cookie::time::Duration;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_redis_store::RedisStore;

/// Default session expiry (24 hours).
pub const DEFAULT_SESSION_EXPIRY_HOURS: i64 = 24;

/// Extended session expiry for "remember me" (30 days).
#[allow(dead_code)]
pub const REMEMBER_ME_SESSION_EXPIRY_DAYS: i64 = 30;

/// Create the session layer using Redis as the backend.
pub async fn create_session_layer(
    redis_url: &str,
    same_site: SameSite,
) -> Result<SessionManagerLayer<RedisStore<Pool>>> {
    let config = Config::from_url(redis_url).context("failed to parse Redis URL")?;

    // Reconnect after Redis restarts, forever, every half second, and give
    // up on a command after five seconds rather than wait on a connection
    // that is gone. Without a policy the pool never reconnects, and every
    // request that touches its session hangs from the restart on.
    let mut builder = Builder::from_config(config);
    builder
        .set_policy(ReconnectPolicy::new_constant(0, 500))
        .with_performance_config(|performance| {
            performance.default_command_timeout = std::time::Duration::from_secs(5);
        });
    let pool = builder
        .build_pool(1)
        .context("failed to create Redis pool")?;

    pool.init()
        .await
        .context("failed to connect to Redis for sessions")?;

    let store = RedisStore::new(pool);

    let session_layer = SessionManagerLayer::new(store)
        .with_secure(true) // Cookie only sent over HTTPS
        .with_http_only(true) // Cookie not accessible via JavaScript
        .with_same_site(same_site)
        .with_expiry(Expiry::OnInactivity(Duration::hours(
            DEFAULT_SESSION_EXPIRY_HOURS,
        )));

    Ok(session_layer)
}
