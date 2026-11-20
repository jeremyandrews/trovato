//! Per user, per plugin mail preference: the unsubscribe token and the opt out
//! for `mail.send-to-user` (BL-94).
//!
//! A row exists once a plugin has mailed a user. Its `token` is what the
//! unsubscribe link in every such mail carries; it authorizes opting this user
//! out of this plugin's mail and nothing else, so it needs no secret key and no
//! expiry. The table is on the plugin protected floor
//! ([`crate::plugin::db_policy`]): only the kernel reads or writes it.

use anyhow::{Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// What a send needs to know about the preference row.
#[derive(Debug, Clone)]
pub struct SendPreference {
    /// The unsubscribe token for this user and plugin.
    pub token: String,
    /// Whether the user opted out of this plugin's mail.
    pub opted_out: bool,
}

/// Read the row for `user` and `plugin`, creating it with a fresh token if
/// this is the plugin's first mail to the user. One statement either way.
pub async fn for_send(pool: &PgPool, user: Uuid, plugin: &str) -> Result<SendPreference> {
    let (token, opted_out_at): (String, Option<DateTime<Utc>>) = sqlx::query_as(
        "INSERT INTO user_mail_preference (user_id, plugin, token) VALUES ($1, $2, $3) \
         ON CONFLICT (user_id, plugin) DO UPDATE SET plugin = EXCLUDED.plugin \
         RETURNING token, opted_out_at",
    )
    .bind(user)
    .bind(plugin)
    .bind(new_token())
    .fetch_one(pool)
    .await
    .context("failed to read or create the mail preference row")?;
    Ok(SendPreference {
        token,
        opted_out: opted_out_at.is_some(),
    })
}

/// The plugin a token unsubscribes from, or `None` for a token that names no
/// row.
pub async fn plugin_for_token(pool: &PgPool, token: &str) -> Result<Option<String>> {
    sqlx::query_scalar("SELECT plugin FROM user_mail_preference WHERE token = $1")
        .bind(token)
        .fetch_optional(pool)
        .await
        .context("failed to look up an unsubscribe token")
}

/// Record the opt out a token authorizes, keeping the first time it was
/// recorded. Returns the plugin, or `None` for a token that names no row.
pub async fn opt_out(pool: &PgPool, token: &str) -> Result<Option<String>> {
    sqlx::query_scalar(
        "UPDATE user_mail_preference SET opted_out_at = COALESCE(opted_out_at, now()) \
         WHERE token = $1 RETURNING plugin",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .context("failed to record an opt out")
}

/// Clear `user`'s opt out of `plugin`'s mail.
pub async fn resubscribe(pool: &PgPool, user: Uuid, plugin: &str) -> Result<()> {
    sqlx::query(
        "UPDATE user_mail_preference SET opted_out_at = NULL WHERE user_id = $1 AND plugin = $2",
    )
    .bind(user)
    .bind(plugin)
    .execute(pool)
    .await
    .context("failed to resubscribe")?;
    Ok(())
}

/// The plugins `user` opted out of, by machine name.
pub async fn opted_out_plugins(pool: &PgPool, user: Uuid) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT plugin FROM user_mail_preference \
         WHERE user_id = $1 AND opted_out_at IS NOT NULL ORDER BY plugin",
    )
    .bind(user)
    .fetch_all(pool)
    .await
    .context("failed to list opted out plugins")
}

/// 32 random bytes from the kernel's usual secure source, URL safe.
fn new_token() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
