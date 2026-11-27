//! Account lockout service using Redis.
//!
//! Tracks failed login attempts and temporarily locks sign-in after too many
//! failures, at two levels:
//!
//! - **Per username and client address.** Five failures from one address in
//!   `ATTEMPT_WINDOW_SECS` lock that address out of that account for
//!   `LOCKOUT_DURATION_SECS`. Someone guessing from one place locks only
//!   themselves out; the owner, signing in from anywhere else, is unaffected.
//! - **Per username, across every address.** `MAX_FAILED_ATTEMPTS_PER_USER`
//!   failures in the same window, from any mix of addresses, lock the account
//!   everywhere. This is the ceiling on guessing spread across many addresses.
//!
//! Every key is built from the username lower-cased and trimmed, in this module
//! itself, because login matches usernames case-insensitively: `Admin`, `admin`
//! and ` ADMIN ` are one account and must be one counter, and no caller can
//! forget to normalise.

use crate::redis_conn::BoundedRedis;
use anyhow::{Context, Result};
use redis::AsyncCommands;
use redis::Client as RedisClient;

/// Failed attempts from one address before that address is locked out of the
/// account.
const MAX_FAILED_ATTEMPTS: u32 = 5;

/// Failed attempts from all addresses together before the account is locked
/// everywhere.
///
/// Fifty in the window: ten times the per-address limit. An owner who mistypes
/// their password cannot plausibly get there, so it never locks a real user out
/// by accident; it takes ten addresses each spending their full five guesses.
/// And it caps guessing spread across many addresses at fifty tries per account
/// per window instead of leaving it unbounded. Lower would hand a small set of
/// addresses the old lock-the-owner-out lever again; higher would let a
/// distributed guesser try more passwords before anything stops them.
const MAX_FAILED_ATTEMPTS_PER_USER: u32 = 50;

/// Lockout duration in seconds (15 minutes).
const LOCKOUT_DURATION_SECS: u64 = 15 * 60;

/// Failed attempt tracking window in seconds (15 minutes).
const ATTEMPT_WINDOW_SECS: u64 = 15 * 60;

/// Account lockout service.
#[derive(Clone)]
pub struct LockoutService {
    redis: RedisClient,
    /// What of an address the lockout warning may log. The Redis keys always
    /// hold the full address: they are counters, and they expire.
    ip_storage: crate::client_addr::ClientIpStorage,
}

impl LockoutService {
    /// Create a new lockout service.
    pub fn new(redis: RedisClient) -> Self {
        Self {
            redis,
            ip_storage: crate::client_addr::ClientIpStorage::Full,
        }
    }

    /// Log only what `CLIENT_IP_STORAGE` allows of an address. The warning is a
    /// `system_log` row, which outlives the lockout.
    #[must_use]
    pub fn with_ip_storage(mut self, ip_storage: crate::client_addr::ClientIpStorage) -> Self {
        self.ip_storage = ip_storage;
        self
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        self.redis
            .bounded_connection()
            .await
            .context("failed to get Redis connection")
    }

    /// Whether sign-in to `username` from `client_ip` is currently locked:
    /// that address is locked out of the account, or the account is locked
    /// everywhere.
    pub async fn is_locked(&self, username: &str, client_ip: &str) -> Result<bool> {
        Ok(self
            .get_lockout_remaining(username, client_ip)
            .await?
            .is_some())
    }

    /// Record a failed sign-in attempt on `username` from `client_ip`.
    ///
    /// Returns (is_now_locked, attempts_remaining) for that address.
    pub async fn record_failed_attempt(
        &self,
        username: &str,
        client_ip: &str,
    ) -> Result<(bool, u32)> {
        let mut conn = self.conn().await?;

        // The account-wide counter first: it counts every failure, whichever
        // address it came from.
        let user_attempts = bump(&mut conn, &attempts_key(username)).await?;
        if user_attempts >= MAX_FAILED_ATTEMPTS_PER_USER {
            conn.set_ex::<_, _, ()>(&lockout_key(username), "locked", LOCKOUT_DURATION_SECS)
                .await
                .context("failed to set account lockout")?;
            conn.del::<_, ()>(&attempts_key(username))
                .await
                .context("failed to clear account attempt counter")?;
            tracing::warn!(
                username = %normalize(username),
                "account locked everywhere after failures from many addresses"
            );
        }

        // Remember which addresses have counters, so `clear_all` can find them.
        let ips = ips_key(username);
        conn.sadd::<_, _, ()>(&ips, client_ip)
            .await
            .context("failed to record the attempting address")?;
        conn.expire::<_, ()>(&ips, (ATTEMPT_WINDOW_SECS + LOCKOUT_DURATION_SECS) as i64)
            .await
            .context("failed to set the address set expiry")?;

        let pair_attempts = bump(&mut conn, &pair_attempts_key(username, client_ip)).await?;
        if pair_attempts >= MAX_FAILED_ATTEMPTS {
            conn.set_ex::<_, _, ()>(
                &pair_lockout_key(username, client_ip),
                "locked",
                LOCKOUT_DURATION_SECS,
            )
            .await
            .context("failed to set lockout")?;
            conn.del::<_, ()>(&pair_attempts_key(username, client_ip))
                .await
                .context("failed to clear attempt counter")?;
            tracing::warn!(
                username = %normalize(username),
                client_ip = %self.ip_storage.kept_or_empty(client_ip),
                "sign-in locked for one address after failed attempts"
            );
            return Ok((true, 0));
        }

        if user_attempts >= MAX_FAILED_ATTEMPTS_PER_USER {
            return Ok((true, 0));
        }
        Ok((false, MAX_FAILED_ATTEMPTS - pair_attempts))
    }

    /// Clear the failed attempts of one address after it signs in successfully.
    ///
    /// Only that address's counter: a success from one place says nothing
    /// about failures from another, and clearing the account-wide counter here
    /// would let a guesser who also knows one valid sign-in reset the ceiling.
    pub async fn clear_attempts(&self, username: &str, client_ip: &str) -> Result<()> {
        let mut conn = self.conn().await?;
        conn.del::<_, ()>(&pair_attempts_key(username, client_ip))
            .await
            .context("failed to clear attempt counter")?;
        Ok(())
    }

    /// Clear all lockout state for a username: every address's attempts and
    /// lock, and the account-wide counter and lock.
    pub async fn clear_all(&self, username: &str) -> Result<()> {
        let mut conn = self.conn().await?;
        let ips: Vec<String> = conn
            .smembers(ips_key(username))
            .await
            .context("failed to read the attempting addresses")?;
        for ip in &ips {
            conn.del::<_, ()>(&pair_attempts_key(username, ip))
                .await
                .context("failed to clear attempt counter")?;
            conn.del::<_, ()>(&pair_lockout_key(username, ip))
                .await
                .context("failed to clear lockout flag")?;
        }
        for key in [
            ips_key(username),
            attempts_key(username),
            lockout_key(username),
        ] {
            conn.del::<_, ()>(&key)
                .await
                .context("failed to clear lockout state")?;
        }
        Ok(())
    }

    /// Remaining lockout time in seconds for `username` from `client_ip`, the
    /// longer of the address lock and the account lock, or `None` if neither.
    pub async fn get_lockout_remaining(
        &self,
        username: &str,
        client_ip: &str,
    ) -> Result<Option<u64>> {
        let mut conn = self.conn().await?;
        let pair: i64 = conn
            .ttl(pair_lockout_key(username, client_ip))
            .await
            .context("failed to get lockout TTL")?;
        let account: i64 = conn
            .ttl(lockout_key(username))
            .await
            .context("failed to get lockout TTL")?;
        let ttl = pair.max(account);
        Ok((ttl > 0).then_some(ttl as u64))
    }
}

/// Increment a windowed counter, starting its window on the first increment.
async fn bump(conn: &mut redis::aio::MultiplexedConnection, key: &str) -> Result<u32> {
    let attempts: u32 = conn
        .incr(key, 1)
        .await
        .context("failed to increment attempt counter")?;
    if attempts == 1 {
        conn.expire::<_, ()>(key, ATTEMPT_WINDOW_SECS as i64)
            .await
            .context("failed to set attempt expiry")?;
    }
    Ok(attempts)
}

/// The form of a username every key is built from.
fn normalize(username: &str) -> String {
    username.trim().to_lowercase()
}

/// Account-wide failure counter.
fn attempts_key(username: &str) -> String {
    format!("lockout:attempts:{}", normalize(username))
}

/// Account-wide lock.
fn lockout_key(username: &str) -> String {
    format!("lockout:locked:{}", normalize(username))
}

/// Failure counter for one address. The address comes first and cannot contain
/// `|`, so the key is unambiguous whatever the username contains.
fn pair_attempts_key(username: &str, client_ip: &str) -> String {
    format!("lockout:pair:attempts:{client_ip}|{}", normalize(username))
}

/// Lock for one address.
fn pair_lockout_key(username: &str, client_ip: &str) -> String {
    format!("lockout:pair:locked:{client_ip}|{}", normalize(username))
}

/// The addresses with failure state for an account, for [`LockoutService::clear_all`].
fn ips_key(username: &str) -> String {
    format!("lockout:ips:{}", normalize(username))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Login matches usernames case-insensitively (`LOWER(name) = LOWER($1)`),
    /// so the lockout must count every spelling of one account together, and a
    /// stray space must not buy a fresh counter either.
    #[test]
    fn every_capitalisation_and_padding_of_a_username_shares_its_keys() {
        for variant in ["Admin", "ADMIN", "admin", " admin", "admin ", "\tAdMiN\n"] {
            assert_eq!(attempts_key(variant), attempts_key("admin"), "{variant:?}");
            assert_eq!(lockout_key(variant), lockout_key("admin"), "{variant:?}");
        }
    }

    #[test]
    fn the_per_address_keys_are_normalised_too_and_keep_addresses_apart() {
        assert_eq!(
            pair_attempts_key(" Admin", "10.0.0.1"),
            pair_attempts_key("admin", "10.0.0.1")
        );
        assert_eq!(
            pair_lockout_key("ADMIN", "::1"),
            pair_lockout_key("admin", "::1")
        );
        assert_ne!(
            pair_attempts_key("admin", "10.0.0.1"),
            pair_attempts_key("admin", "10.0.0.2")
        );
        assert_ne!(
            pair_attempts_key("admin", "10.0.0.1"),
            attempts_key("admin")
        );
    }

    /// Guard: already true before normalisation; kept so lower-casing can never
    /// merge two different accounts.
    #[test]
    fn different_usernames_keep_different_keys() {
        assert_ne!(attempts_key("alice"), attempts_key("bob"));
        assert_ne!(lockout_key("alice"), lockout_key("bob"));
    }
}
