//! The item view counter: how many times each item has been viewed (B160).
//!
//! Drupal's statistics module counted node views and showed "N reads" under a
//! post to roles holding `view post access counter`. Trovato counts the same
//! way, in `item_counter`, when the site turns it on: "Count content views" at
//! `/admin/config/site`, off by default, because it costs one write per item
//! view.
//!
//! Whether counting is on is read from `site_config` once and kept until a
//! `SiteConfig` cache generation bump or `SETTING_TTL`, so an item view with
//! counting off costs no query at all, and one with it on costs the one upsert
//! that both counts the view and returns the new total.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

/// The `site_config` key: a boolean, absent meaning off.
pub const SETTING_KEY: &str = "count_content_views";

/// The permission that shows the count on an item's page.
pub const VIEW_COUNT_PERMISSION: &str = "view post access counter";

/// How long the setting is kept before it is read again, on a server that
/// missed the cache generation bump a save makes.
const SETTING_TTL: Duration = Duration::from_secs(60);

/// Whether the site counts views, as stored. Off when absent or unreadable.
pub async fn stored_setting(pool: &PgPool) -> bool {
    crate::models::SiteConfig::get(pool, SETTING_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// The counter, with its cached on/off setting.
#[derive(Debug, Default)]
pub struct ViewCounter {
    enabled: parking_lot::RwLock<Option<(bool, Instant)>>,
}

impl ViewCounter {
    /// Whether the site counts views, from the cache.
    pub async fn enabled(&self, pool: &PgPool) -> bool {
        if let Some((enabled, loaded)) = *self.enabled.read()
            && loaded.elapsed() < SETTING_TTL
        {
            return enabled;
        }
        let enabled = stored_setting(pool).await;
        *self.enabled.write() = Some((enabled, Instant::now()));
        enabled
    }

    /// Forget the cached setting, so the next view reads it again.
    pub fn clear(&self) {
        *self.enabled.write() = None;
    }

    /// Count one view of `item_id` and return its total, or `None` when the
    /// site does not count views. One statement: the upsert returns the total.
    pub async fn record(&self, pool: &PgPool, item_id: Uuid) -> Result<Option<i64>> {
        if !self.enabled(pool).await {
            return Ok(None);
        }
        let total: i64 = sqlx::query_scalar(
            "INSERT INTO item_counter (item_id, total_count, last_viewed) \
             VALUES ($1, 1, EXTRACT(EPOCH FROM NOW())::BIGINT) \
             ON CONFLICT (item_id) DO UPDATE \
             SET total_count = item_counter.total_count + 1, \
                 last_viewed = EXCLUDED.last_viewed \
             RETURNING total_count",
        )
        .bind(item_id)
        .fetch_one(pool)
        .await
        .context("failed to count an item view")?;
        Ok(Some(total))
    }
}
