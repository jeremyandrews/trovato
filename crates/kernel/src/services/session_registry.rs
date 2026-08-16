//! The per-user session index (FR-7b, design §3, D-36).
//!
//! tower-sessions stores sessions as opaque Redis entries keyed by session id.
//! There is no user→sessions mapping, so "show me my active devices" and "log
//! that other device out" are unanswerable. This module adds exactly that
//! mapping and nothing else: the cookie, the expiry, CSRF, and `SESSION_USER_ID`
//! semantics are untouched. We extend tower-sessions; we do not fork it.
//!
//! # Keyed by device, not by session id
//!
//! The index is a Redis hash `user_sessions:{user_id}` whose fields are a
//! **device id** — a UUID minted once and stored *inside the session data*.
//!
//! That indirection is what makes the D-36 requirement "migrated on `cycle_id`"
//! actually work. `Session::cycle_id()` sets the in-memory id to `None` and the
//! new id is only assigned when the session layer saves, which happens *outside*
//! any middleware we can install — so no code of ours can observe the new id in
//! the request that cycled it. The device id, by contrast, lives in the session
//! *record*, which `cycle_id` preserves. The next request presents the same
//! device id with a new session id, and the entry updates in place: one row per
//! device across its whole lifetime, exactly as the device list should read.
//!
//! # Revocation is authoritative here, not in the store
//!
//! Revoking deletes the index field **and** best-effort deletes the underlying
//! tower-sessions record. The index deletion is the authoritative half: the
//! session-tracking middleware refuses any request whose session was registered
//! but no longer has an index entry, and destroys that session. So a revoked
//! session's next request fails even if the recorded session id was one request
//! stale when the store delete went out. Correctness does not depend on winning
//! that race.
//!
//! # Not durable past the TTL — that is what the audit stream is for
//!
//! Entries expire with the session (D-36 accepts no durable post-TTL registry).
//! The **audit stream** carries session lifecycle durably (the D-36 rider), so
//! incident response still has a record after the registry has forgotten.

use anyhow::{Context, Result};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Session key holding this browser's stable device id.
///
/// Lives in the session *record*, so it survives `cycle_id` and is what the
/// index is keyed by. See the module docs for why the session id cannot be.
pub const SESSION_DEVICE_ID: &str = "device_id";

/// Session key marking that this session has been entered into the index.
///
/// Distinguishes "brand new session, not registered yet" from "was registered,
/// and its entry is now gone" — the latter is a revocation and must terminate
/// the session.
pub const SESSION_REGISTERED: &str = "session_registered";

/// How stale `last_seen` may get before a request refreshes it.
///
/// Without a floor this would be a Redis write on every single request. A minute
/// is precise enough for a device list and cheap enough to be invisible.
pub const LAST_SEEN_THROTTLE_SECS: i64 = 60;

/// The Redis key holding one user's session index.
fn index_key(user_id: Uuid) -> String {
    format!("user_sessions:{user_id}")
}

/// One active session, as the device list renders it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Stable per-device identifier; what revocation addresses.
    pub device_id: Uuid,
    /// The session id currently backing this device. Used to delete the
    /// underlying tower-sessions record; may lag by one request immediately
    /// after a `cycle_id` (see the module docs).
    pub session_id: String,
    /// User-Agent-derived label, renameable by the user.
    pub device_name: String,
    /// Raw User-Agent, kept for the "is this really my device?" question.
    pub user_agent: String,
    /// Vetted client IP (`ClientIp`) last seen on this session.
    pub ip: String,
    /// Unix seconds when the session was first indexed.
    pub created_at: i64,
    /// Unix seconds of the last request, throttled to
    /// [`LAST_SEEN_THROTTLE_SECS`].
    pub last_seen: i64,
}

/// What [`SessionRegistry::observe`] concluded about a request's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// A session appeared in the index for the first time.
    Registered,
    /// The device already had an entry and its session id changed — the
    /// fixation defence fired and we migrated the entry onto the new id.
    Cycled,
    /// An ordinary request; `last_seen` may or may not have been refreshed.
    Seen,
    /// The session was registered before and its entry is gone: it was revoked.
    /// The caller must terminate this session.
    Revoked,
}

/// Derive a human label from a `User-Agent`.
///
/// Deliberately coarse. The point is to let someone recognise "that is my work
/// laptop" in a list, not to fingerprint the client, and a coarse label is both
/// more readable and less of a tracking surface than the raw string (which is
/// kept separately for anyone who wants it).
pub fn device_name_from_user_agent(ua: &str) -> String {
    if ua.trim().is_empty() {
        return "Unknown device".to_string();
    }

    // Order matters: several browsers include "Safari" or "Chrome" in their UA
    // for compatibility, so the more specific brand has to win.
    let browser = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("OPR/") || ua.contains("Opera") {
        "Opera"
    } else if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Chrome/") || ua.contains("Chromium/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "Browser"
    };

    let platform = if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") {
        "iPad"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        return browser.to_string();
    };

    format!("{browser} on {platform}")
}

/// Whether `last_seen` is stale enough to be worth a Redis write.
pub fn should_refresh_last_seen(last_seen: i64, now: i64) -> bool {
    now - last_seen >= LAST_SEEN_THROTTLE_SECS
}

/// The per-user session index.
#[derive(Clone)]
pub struct SessionRegistry {
    redis: redis::Client,
    /// Seconds an index entry lives without a refresh. Matches the session's own
    /// inactivity expiry so the two cannot disagree about what is still alive.
    ttl_secs: i64,
}

impl SessionRegistry {
    /// Create the registry over the kernel's Redis client.
    pub fn new(redis: redis::Client, ttl_secs: i64) -> Self {
        Self { redis, ttl_secs }
    }

    /// Read one user's active sessions, most recently seen first.
    pub async fn list(&self, user_id: Uuid) -> Result<Vec<SessionEntry>> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis for the session index")?;

        let raw: std::collections::HashMap<String, String> = conn
            .hgetall(index_key(user_id))
            .await
            .context("failed to read the session index")?;

        let mut entries: Vec<SessionEntry> = raw
            .values()
            // A malformed field is skipped rather than failing the whole list:
            // a device list that renders nothing because one entry is corrupt is
            // worse than one that renders the rest.
            .filter_map(|v| serde_json::from_str(v).ok())
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.last_seen));
        Ok(entries)
    }

    /// Read one entry by device id.
    pub async fn get(&self, user_id: Uuid, device_id: Uuid) -> Result<Option<SessionEntry>> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis for the session index")?;

        let raw: Option<String> = conn
            .hget(index_key(user_id), device_id.to_string())
            .await
            .context("failed to read a session index entry")?;

        Ok(raw.and_then(|v| serde_json::from_str(&v).ok()))
    }

    /// Write an entry and refresh the index TTL.
    async fn put(&self, user_id: Uuid, entry: &SessionEntry) -> Result<()> {
        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis for the session index")?;

        let encoded = serde_json::to_string(entry).context("failed to encode a session entry")?;
        let key = index_key(user_id);

        let _: () = conn
            .hset(&key, entry.device_id.to_string(), encoded)
            .await
            .context("failed to write a session index entry")?;
        // The whole index expires with the longest-lived session in it; each
        // write pushes it out again, so an active user's list never vanishes
        // underneath them.
        let _: () = conn
            .expire(&key, self.ttl_secs)
            .await
            .context("failed to set the session index TTL")?;
        Ok(())
    }

    /// Record a request against a session, returning what it implied.
    ///
    /// This is the single write path: registration, `cycle_id` migration, and
    /// the throttled `last_seen` touch are all the same upsert, which is why
    /// there is no way for them to disagree.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe(
        &self,
        user_id: Uuid,
        device_id: Uuid,
        session_id: &str,
        ip: &str,
        user_agent: &str,
        was_registered: bool,
        now: i64,
    ) -> Result<Observation> {
        let existing = self.get(user_id, device_id).await?;

        let Some(mut entry) = existing else {
            if was_registered {
                // The session says it was indexed, and it is not. Someone
                // revoked it. Fail closed: the caller terminates the session.
                return Ok(Observation::Revoked);
            }
            let entry = SessionEntry {
                device_id,
                session_id: session_id.to_string(),
                device_name: device_name_from_user_agent(user_agent),
                user_agent: user_agent.to_string(),
                ip: ip.to_string(),
                created_at: now,
                last_seen: now,
            };
            self.put(user_id, &entry).await?;
            return Ok(Observation::Registered);
        };

        if entry.session_id != session_id {
            // The fixation defence fired since we last saw this device. Migrate
            // the entry onto the new id rather than orphaning the old one and
            // showing a phantom device.
            entry.session_id = session_id.to_string();
            entry.ip = ip.to_string();
            entry.last_seen = now;
            self.put(user_id, &entry).await?;
            return Ok(Observation::Cycled);
        }

        if should_refresh_last_seen(entry.last_seen, now) {
            entry.last_seen = now;
            entry.ip = ip.to_string();
            self.put(user_id, &entry).await?;
        }
        Ok(Observation::Seen)
    }

    /// Rename a device, scoped to its owner.
    pub async fn rename(&self, user_id: Uuid, device_id: Uuid, name: &str) -> Result<bool> {
        let Some(mut entry) = self.get(user_id, device_id).await? else {
            return Ok(false);
        };
        entry.device_name = name.to_string();
        self.put(user_id, &entry).await?;
        Ok(true)
    }

    /// Revoke one session: drop the index entry and kill the stored session.
    ///
    /// Returns the entry that was revoked, so the caller can audit what it was.
    /// The index deletion is what makes the revocation stick (see the module
    /// docs); the store delete is what makes it take effect immediately rather
    /// than on the next request.
    pub async fn revoke(&self, user_id: Uuid, device_id: Uuid) -> Result<Option<SessionEntry>> {
        let Some(entry) = self.get(user_id, device_id).await? else {
            return Ok(None);
        };

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis for the session index")?;

        let _: () = conn
            .hdel(index_key(user_id), device_id.to_string())
            .await
            .context("failed to delete a session index entry")?;

        // tower-sessions-redis-store keys records by the bare session id string.
        // Best-effort: a stale id here is covered by the index deletion above.
        let _: Result<i64, _> = conn.del(&entry.session_id).await;

        Ok(Some(entry))
    }

    /// Revoke every session for a user except one.
    ///
    /// Returns the entries revoked. This is the "log out everywhere else"
    /// action, and deliberately keeps the caller's own session alive so the
    /// action does not log them out of the page they performed it from.
    pub async fn revoke_all_except(
        &self,
        user_id: Uuid,
        keep_device_id: Option<Uuid>,
    ) -> Result<Vec<SessionEntry>> {
        let entries = self.list(user_id).await?;
        let mut revoked = Vec::new();
        for entry in entries {
            if Some(entry.device_id) == keep_device_id {
                continue;
            }
            if let Some(e) = self.revoke(user_id, entry.device_id).await? {
                revoked.push(e);
            }
        }
        Ok(revoked)
    }
}

impl std::fmt::Debug for SessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRegistry")
            .field("ttl_secs", &self.ttl_secs)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn device_names_are_readable() {
        let chrome_mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
        assert_eq!(device_name_from_user_agent(chrome_mac), "Chrome on macOS");

        let firefox_linux =
            "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0";
        assert_eq!(
            device_name_from_user_agent(firefox_linux),
            "Firefox on Linux"
        );

        let safari_iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) \
                             AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 \
                             Mobile/15E148 Safari/604.1";
        assert_eq!(
            device_name_from_user_agent(safari_iphone),
            "Safari on iPhone"
        );
    }

    #[test]
    fn the_more_specific_brand_wins() {
        // Edge and Opera both ship "Chrome/" and "Safari/" in their UA for
        // compatibility; naive matching would label every Edge user "Chrome".
        let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like \
                    Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0";
        assert_eq!(device_name_from_user_agent(edge), "Edge on Windows");

        let opera = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like \
                     Gecko) Chrome/119.0.0.0 Safari/537.36 OPR/105.0.0.0";
        assert_eq!(device_name_from_user_agent(opera), "Opera on Windows");
    }

    #[test]
    fn an_absent_user_agent_is_labelled_not_blank() {
        assert_eq!(device_name_from_user_agent(""), "Unknown device");
        assert_eq!(device_name_from_user_agent("   "), "Unknown device");
    }

    #[test]
    fn an_unrecognized_user_agent_still_gets_a_label() {
        // curl, a bot, a native client — never an empty row in the list.
        assert_eq!(device_name_from_user_agent("curl/8.4.0"), "Browser");
    }

    #[test]
    fn last_seen_is_throttled() {
        let now = 1_000_000;
        // A burst of requests inside the window must not write.
        assert!(!should_refresh_last_seen(now, now));
        assert!(!should_refresh_last_seen(now, now + 1));
        assert!(!should_refresh_last_seen(
            now,
            now + LAST_SEEN_THROTTLE_SECS - 1
        ));
        // At the boundary and beyond, refresh.
        assert!(should_refresh_last_seen(now, now + LAST_SEEN_THROTTLE_SECS));
        assert!(should_refresh_last_seen(now, now + 3600));
    }

    #[test]
    fn the_index_key_is_namespaced_per_user() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert_ne!(index_key(a), index_key(b));
        assert!(index_key(a).starts_with("user_sessions:"));
    }

    #[test]
    fn entries_round_trip_as_json() {
        let entry = SessionEntry {
            device_id: Uuid::now_v7(),
            session_id: "abc".into(),
            device_name: "Chrome on macOS".into(),
            user_agent: "…".into(),
            ip: "127.0.0.1".into(),
            created_at: 1,
            last_seen: 2,
        };
        let encoded = serde_json::to_string(&entry).unwrap();
        let back: SessionEntry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(back.device_id, entry.device_id);
        assert_eq!(back.session_id, entry.session_id);
        assert_eq!(back.last_seen, entry.last_seen);
    }
}
