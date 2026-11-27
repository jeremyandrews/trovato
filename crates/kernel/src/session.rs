//! Session management using Redis.
//!
//! The session cookie is the only cookie the kernel sets. CSRF tokens, the
//! "remember me" flag and the signed-in user all live inside the session, in
//! Redis, not in cookies of their own. So the session cookie's flags, set here,
//! are the flags of every cookie the kernel sends.

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

/// Whether the session cookie carries `Secure` (`COOKIE_SECURE`).
///
/// Read once at startup. `always`, the default, is right for every site
/// served over HTTPS, directly or behind a TLS-terminating proxy. `never` is
/// for a site served over plain HTTP on a LAN or a development machine:
/// a browser drops a `Secure` cookie on plain HTTP (except on `localhost`),
/// so without it nobody can log in. It is refused at startup when `SITE_URL`
/// is `https://`, and logged as a warning whenever it is set.
///
/// There is no `auto`. The session layer's flags are fixed when it is built,
/// so a per-request choice would mean a second layer or rewriting
/// `Set-Cookie` on every response, and getting the scheme right behind a
/// proxy means trusting `X-Forwarded-Proto`. An explicit setting is simpler
/// and cannot be talked out of `Secure` by a header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CookieSecure {
    /// Every cookie is `Secure`. The default.
    #[default]
    Always,
    /// No cookie is `Secure`. Plain HTTP LAN or development sites only.
    Never,
}

impl CookieSecure {
    /// Parse `always` or `never` (case-insensitive). Empty is the default.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "always" | "" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(format!(
                "`{other}` is not a cookie secure setting (always or never)"
            )),
        }
    }

    /// The setting's value, as `COOKIE_SECURE` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    /// Whether cookies carry the `Secure` attribute.
    pub fn is_secure(self) -> bool {
        self == Self::Always
    }
}

/// Create the session layer using Redis as the backend.
pub async fn create_session_layer(
    redis_url: &str,
    same_site: SameSite,
    cookie_secure: CookieSecure,
) -> Result<SessionManagerLayer<RedisStore<Pool>>> {
    let config = Config::from_url(redis_url).context("failed to parse Redis URL")?;

    // Reconnect after Redis restarts, forever, every half second, and give
    // up on a connect or a command after the same bounds every other kernel
    // Redis connection has (`redis_conn`), rather than wait on a connection
    // that is gone. Without a policy the pool never reconnects, and every
    // request that touches its session hangs from the restart on.
    let mut builder = Builder::from_config(config);
    builder
        .set_policy(ReconnectPolicy::new_constant(0, 500))
        .with_connection_config(|connection| {
            connection.connection_timeout = crate::redis_conn::REDIS_CONNECT_TIMEOUT;
        })
        .with_performance_config(|performance| {
            performance.default_command_timeout = crate::redis_conn::REDIS_RESPONSE_TIMEOUT;
        });
    let pool = builder
        .build_pool(1)
        .context("failed to create Redis pool")?;

    pool.init()
        .await
        .context("failed to connect to Redis for sessions")?;

    let store = RedisStore::new(pool);

    let session_layer = SessionManagerLayer::new(store)
        .with_secure(cookie_secure.is_secure()) // COOKIE_SECURE, default always
        .with_http_only(true) // Cookie not accessible via JavaScript
        .with_same_site(same_site)
        .with_expiry(Expiry::OnInactivity(Duration::hours(
            DEFAULT_SESSION_EXPIRY_HOURS,
        )));

    Ok(session_layer)
}
