//! Session management using Redis.

use anyhow::{Context, Result};
use fred::prelude::*;
use tower_sessions::cookie::time::Duration;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_redis_store::RedisStore;

/// Default session expiry (24 hours).
pub const DEFAULT_SESSION_EXPIRY_HOURS: i64 = 24;

/// Extended session expiry for "remember me" (30 days).
#[allow(dead_code)]
pub const REMEMBER_ME_SESSION_EXPIRY_DAYS: i64 = 30;

/// Create the session layer using Redis as the backend.
pub async fn create_session_layer(redis_url: &str) -> Result<SessionManagerLayer<RedisStore<Pool>>> {
    let config = Config::from_url(redis_url)
        .context("failed to parse Redis URL")?;

    let pool = Builder::from_config(config)
        .build_pool(1)
        .context("failed to create Redis pool")?;

    pool.init()
        .await
        .context("failed to connect to Redis for sessions")?;

    let store = RedisStore::new(pool);

    let session_layer = SessionManagerLayer::new(store)
        .with_secure(true)           // Cookie only sent over HTTPS
        .with_http_only(true)        // Cookie not accessible via JavaScript
        .with_same_site(SameSite::Strict) // Cookie only sent for same-site requests
        .with_expiry(Expiry::OnInactivity(Duration::hours(DEFAULT_SESSION_EXPIRY_HOURS)));

    Ok(session_layer)
}
