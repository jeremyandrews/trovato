//! Reading feed and topic **configuration** from the Item tier (M3).
//!
//! `M3-DESIGN.md` Decision 1 moved feed and topic configuration onto Items so an
//! admin can manage them through the kernel's generic content forms — the only
//! writable surface the frozen contract offers a plugin. Decision 2 kept a
//! feed's mutable fetch state in the plugin-owned `argus_feeds` table, keyed by
//! the feed Item's id.
//!
//! This module is the read half of that split. Parsing and validation are pure
//! and live in [`argus_core::config`]; what is here is the host traffic: which
//! item-api call answers which question, how a page of feeds is assembled, and
//! the one-shot backfill that carries M1/M2 rows across.

use argus_core::config::{self, FeedConfig, TopicConfig};
use argus_core::notify::ChannelConfig;
use argus_core::{CoreError, CoreResult};
use serde::Deserialize;
use serde_json::{Value, json};
use trovato_sdk::host;

use crate::host_ports::{exec, query_rows};
use crate::item_host;

/// Item status meaning "published" — which, for a feed or a topic, means
/// enabled. Using the Item's own status rather than a second `enabled` field
/// keeps one switch instead of two that can disagree, and makes the admin's
/// publish checkbox do the obvious thing.
pub const STATUS_ENABLED: i64 = 1;

/// Page size for `query-items`. The host clamps `limit` to 100
/// (`crates/kernel/src/host/item.rs`, `query-items`), so this is that ceiling
/// named rather than a number that silently becomes a different number.
const MAX_CONFIG_PAGE: i64 = 100;

/// Hard bound on configuration paging, as a runaway guard: 100 pages is 10,000
/// feeds, far past any plausible install, and a cron tick that somehow looped
/// would otherwise burn its whole epoch here.
const MAX_CONFIG_PAGES: usize = 100;

/// `argus_state` key marking the legacy-configuration backfill as done.
const BACKFILL_KEY: &str = "config_backfill_v3";

/// `argus_state` key prefix recording "this legacy id became that Item id".
///
/// The map is what makes the backfill resumable. There are no transactions
/// (G-DB-NO-TX), so a pass can die between creating an Item and repointing the
/// rows that referenced the old id; on the next cycle the map says which rows
/// are already done.
const MAPPING_PREFIX: &str = "config_migrated:";

/// Map an item-api host error code into a transient store error.
///
/// Transient is the right default: every failure mode behind these codes (no
/// services, SQL failure, a truncated buffer) is one a queue retry can clear,
/// and a config read that fails must not be mistaken for "this feed does not
/// exist", which would silently stop fetching it.
fn map_item_err(code: i32) -> CoreError {
    CoreError::Store(format!("item host error {code}"))
}

/// Load one feed's configuration by its Item id.
///
/// Returns `None` when the Item is missing, is not an `argus_feed`, or is
/// unpublished — the three ways a feed can be "not currently a feed", all of
/// which the fetch stage treats identically.
///
/// # Errors
///
/// Transient [`CoreError::Store`] when the item host call fails.
pub fn load_feed_config(feed_id: &str) -> CoreResult<Option<FeedConfig>> {
    let item = item_host::get_item(feed_id).map_err(map_item_err)?;
    Ok(enabled_item(&item, config::FEED_TYPE)
        .and_then(|v| config::parse_feed(&v))
        // Same filter as the scheduler: a job enqueued before the URL was
        // broken resolves to "no such feed" rather than a fetch of nothing.
        .filter(|f| config::normalize_url(&f.url).is_some()))
}

/// Load one topic's configuration by its Item id.
///
/// # Errors
///
/// Transient [`CoreError::Store`] when the item host call fails.
pub fn load_topic_config(topic_id: &str) -> CoreResult<Option<TopicConfig>> {
    if topic_id.is_empty() {
        return Ok(None);
    }
    let item = item_host::get_item(topic_id).map_err(map_item_err)?;
    // A topic is read for its prompt and threshold even when unpublished: an
    // admin pausing a topic should stop new fetches, not change how articles
    // already in flight are scored. Only the type has to match.
    Ok(typed_item(&item, config::TOPIC_TYPE).and_then(|v| config::parse_topic(&v)))
}

/// Load every published feed's configuration that is actually fetchable, paging
/// through `query-items`.
///
/// Feeds whose URL is unusable are dropped here rather than scheduled and left
/// to fail on every tick. This is where the rejection `tap_item_presave` could
/// not perform actually lands: presave can only rewrite `fields`, so it blanks
/// the bad URL and writes the reason to the note field, and this filter is what
/// stops the feed being polled (`G-NO-PRESAVE-VETO`).
///
/// # Errors
///
/// Transient [`CoreError::Store`] when the item host call fails.
pub fn load_enabled_feed_configs() -> CoreResult<Vec<FeedConfig>> {
    let mut out = Vec::new();
    for page in 0..MAX_CONFIG_PAGES {
        let query = json!({
            "type": config::FEED_TYPE,
            "status": STATUS_ENABLED,
            "limit": MAX_CONFIG_PAGE,
            "offset": page as i64 * MAX_CONFIG_PAGE,
        });
        let items = item_host::query_items(&query).map_err(map_item_err)?;
        let rows = items.as_array().cloned().unwrap_or_default();
        let received = rows.len();
        out.extend(
            rows.iter()
                .filter_map(config::parse_feed)
                .filter(|f| config::normalize_url(&f.url).is_some()),
        );
        if (received as i64) < MAX_CONFIG_PAGE {
            return Ok(out);
        }
    }
    // Reaching the page cap means the install has more feeds than this loop will
    // read. Say so rather than silently scheduling a prefix of them.
    host::log(
        "warning",
        "argus",
        &format!(
            "feed configuration paging hit the {MAX_CONFIG_PAGES}-page cap; \
             feeds past {} are not being scheduled",
            MAX_CONFIG_PAGES as i64 * MAX_CONFIG_PAGE
        ),
    );
    Ok(out)
}

/// Load every enabled notification channel's configuration (M4).
///
/// Published means enabled, as it does for a feed. Channels whose configuration
/// cannot address anything — no recognized kind, no target — are dropped by
/// [`config::parse_channel`] rather than handed to the dispatcher, which is the
/// same place a broken feed URL is stopped and for the same reason: presave
/// cannot refuse the save that created them.
///
/// The order is whatever `query-items` returns, made stable by sorting on the
/// channel id, because the dispatch cap takes a prefix of this list and a cap
/// that took a different prefix on every run would silently rotate which
/// channels get their notification in-job and which get their own job.
///
/// # Errors
///
/// Transient [`CoreError::Store`] when the item host call fails.
pub fn load_enabled_channel_configs() -> CoreResult<Vec<ChannelConfig>> {
    let mut out: Vec<ChannelConfig> = Vec::new();
    for page in 0..MAX_CONFIG_PAGES {
        let query = json!({
            "type": config::CHANNEL_TYPE,
            "status": STATUS_ENABLED,
            "limit": MAX_CONFIG_PAGE,
            "offset": page as i64 * MAX_CONFIG_PAGE,
        });
        let items = item_host::query_items(&query).map_err(map_item_err)?;
        let rows = items.as_array().cloned().unwrap_or_default();
        let received = rows.len();
        out.extend(rows.iter().filter_map(config::parse_channel));
        if (received as i64) < MAX_CONFIG_PAGE {
            break;
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Load one notification channel by its Item id, or `None` when it is gone,
/// unpublished, or no longer usable.
///
/// # Errors
///
/// Transient [`CoreError::Store`] when the item host call fails.
pub fn load_channel_config(channel_id: &str) -> CoreResult<Option<ChannelConfig>> {
    let item = item_host::get_item(channel_id).map_err(map_item_err)?;
    Ok(enabled_item(&item, config::CHANNEL_TYPE).and_then(|v| config::parse_channel(&v)))
}

/// Narrow an item-api response to a published Item of the expected type.
fn enabled_item(item: &Value, expected_type: &str) -> Option<Value> {
    let typed = typed_item(item, expected_type)?;
    (typed.get("status").and_then(Value::as_i64) == Some(STATUS_ENABLED)).then_some(typed)
}

/// Narrow an item-api response to an Item of the expected type.
///
/// The type check is not ceremony: `get-item` takes a bare id, so nothing stops
/// a stale `feed_id` from resolving to some other plugin's Item, and parsing
/// that as a feed would produce a config full of defaults rather than an error.
fn typed_item(item: &Value, expected_type: &str) -> Option<Value> {
    let obj = item.as_object()?;
    (obj.get("type").and_then(Value::as_str) == Some(expected_type)).then(|| item.clone())
}

// ---------------------------------------------------------------------------
// One-shot backfill of M1/M2 configuration rows
// ---------------------------------------------------------------------------

/// What one backfill pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Topics carried across to Items.
    pub topics: usize,
    /// Feeds carried across to Items.
    pub feeds: usize,
    /// `true` when the pass was skipped because it had already run.
    pub already_done: bool,
}

#[derive(Deserialize)]
struct LegacyTopicRow {
    id: String,
    name: String,
    relevance_prompt: String,
    relevance_threshold: i64,
    enabled: bool,
}

#[derive(Deserialize)]
struct LegacyFeedRow {
    id: String,
    url: String,
    name: String,
    topic_id: String,
    fetch_interval_seconds: i64,
    enabled: bool,
}

#[derive(Deserialize)]
struct CountRow {
    n: i64,
}

/// Carry M1/M2 feed and topic rows into the Item tier, once.
///
/// # Why this is a tap and not a migration
///
/// `item.type` is `NOT NULL REFERENCES item_type(type)`, and the `item_type`
/// rows for a plugin's declared types are written at runtime by
/// `ContentTypeRegistry::sync_from_plugins`, which runs *after* migrations. A
/// migration that inserted an `argus_feed` Item would violate that foreign key
/// every time. `M3-DESIGN.md` Decision 3.
///
/// # Why the ids change
///
/// `save-item` reads a non-nil `id` in its payload as "update this Item", so
/// creating an Item with a chosen id is not expressible through the item host
/// (`G-ITEM-NO-CREATE-WITH-ID`). Rather than write the `item` table directly
/// through the `db` host — which `raw_sql` would permit and which would skip the
/// kernel's own creation path — each legacy row gets a fresh Item and the
/// argus-owned rows that referenced the old id are repointed at the new one. The
/// rewriting is confined to this plugin's own tables.
///
/// Idempotent and resumable: a marker in `argus_state` short-circuits the whole
/// pass, and each legacy id is recorded in the mapping as it is carried, so an
/// interrupted pass resumes rather than duplicating. Safe to call on every cron
/// cycle, which is exactly how it is called.
///
/// # Errors
///
/// Transient [`CoreError::Store`] when a legacy read or an Item write fails.
/// The caller logs and continues: a failed backfill must not stop the pipeline,
/// and the next cycle retries it.
pub fn backfill_legacy_config() -> CoreResult<BackfillReport> {
    if backfill_done()? {
        return Ok(BackfillReport {
            already_done: true,
            ..BackfillReport::default()
        });
    }

    // Topics first: a feed Item's topic field names a topic Item, so the topic
    // mapping has to exist before the feeds that reference it are carried.
    let topics: Vec<LegacyTopicRow> = query_rows(
        "SELECT id::text AS id, name, relevance_prompt, relevance_threshold, enabled \
         FROM argus_topics ORDER BY created",
        &[],
    )?;
    let mut topic_count = 0usize;
    for row in topics {
        if mapped_id(&row.id)?.is_some() {
            continue;
        }
        let new_id = create_config_item(
            config::TOPIC_TYPE,
            &row.name,
            row.enabled,
            json!({
                config::FIELD_RELEVANCE_PROMPT: row.relevance_prompt,
                config::FIELD_RELEVANCE_THRESHOLD: config::clamp_threshold(row.relevance_threshold),
            }),
        )?;
        record_mapping(&row.id, &new_id)?;
        exec(
            "UPDATE argus_articles SET topic_id = $2::uuid WHERE topic_id = $1::uuid",
            &[json!(row.id), json!(new_id)],
        )?;
        topic_count += 1;
    }

    // Only rows that still carry M1/M2 configuration. From M3 on, `argus_feeds`
    // is a state-only table whose rows are created on demand by the first fetch
    // with every configuration column NULL — and those columns are not nullable
    // in this row type, so selecting them all would fail to decode and the
    // backfill would error on every cron tick for the life of the install
    // (observed as `invalid type: null, expected a string`). The `NOT NULL`
    // filter is what tells a legacy row from a state row.
    let feeds: Vec<LegacyFeedRow> = query_rows(
        "SELECT id::text AS id, url, name, topic_id::text AS topic_id, \
                fetch_interval_seconds, enabled \
         FROM argus_feeds \
         WHERE url IS NOT NULL AND name IS NOT NULL AND topic_id IS NOT NULL \
         ORDER BY created",
        &[],
    )?;
    let mut feed_count = 0usize;
    for row in feeds {
        if mapped_id(&row.id)?.is_some() {
            continue;
        }
        // A feed carried in an earlier stage of this same pass points at the
        // topic's new Item; one whose topic was already an Item id maps to
        // itself and is left alone.
        let topic_id = mapped_id(&row.topic_id)?.unwrap_or(row.topic_id);
        let new_id = create_config_item(
            config::FEED_TYPE,
            &row.name,
            row.enabled,
            json!({
                config::FIELD_URL: row.url,
                config::FIELD_TOPIC: topic_id,
                config::FIELD_FETCH_INTERVAL: config::clamp_interval(row.fetch_interval_seconds),
            }),
        )?;
        record_mapping(&row.id, &new_id)?;
        exec(
            "UPDATE argus_articles SET feed_id = $2::uuid WHERE feed_id = $1::uuid",
            &[json!(row.id), json!(new_id)],
        )?;
        // The state row is keyed by the feed Item's id from M3 on, so its
        // primary key moves with the feed. Nothing references it, so this is a
        // plain update rather than a cascade.
        exec(
            "UPDATE argus_feeds SET id = $2::uuid WHERE id = $1::uuid",
            &[json!(row.id), json!(new_id)],
        )?;
        // The re-entry guard: after the update above, re-reading `argus_feeds`
        // yields the *new* id, which is not a mapping key. Mapping it to itself
        // makes the row self-identify as carried on any later pass.
        record_mapping(&new_id, &new_id)?;
        feed_count += 1;
    }

    mark_backfill_done()?;
    Ok(BackfillReport {
        topics: topic_count,
        feeds: feed_count,
        already_done: false,
    })
}

/// Whether the backfill marker is already set.
fn backfill_done() -> CoreResult<bool> {
    let rows: Vec<CountRow> = query_rows(
        "SELECT COUNT(*)::bigint AS n FROM argus_state WHERE name = $1",
        &[json!(BACKFILL_KEY)],
    )?;
    Ok(rows.first().is_some_and(|r| r.n > 0))
}

/// Set the backfill marker.
fn mark_backfill_done() -> CoreResult<()> {
    exec(
        "INSERT INTO argus_state (name, value) VALUES ($1, 'done') \
         ON CONFLICT (name) DO NOTHING",
        &[json!(BACKFILL_KEY)],
    )?;
    Ok(())
}

/// The Item id a legacy id was carried to, if it has been.
fn mapped_id(legacy_id: &str) -> CoreResult<Option<String>> {
    #[derive(Deserialize)]
    struct ValueRow {
        value: String,
    }
    let rows: Vec<ValueRow> = query_rows(
        "SELECT value FROM argus_state WHERE name = $1",
        &[json!(format!("{MAPPING_PREFIX}{legacy_id}"))],
    )?;
    Ok(rows.into_iter().next().map(|r| r.value))
}

/// Record that `legacy_id` was carried to `new_id`.
fn record_mapping(legacy_id: &str, new_id: &str) -> CoreResult<()> {
    exec(
        "INSERT INTO argus_state (name, value) VALUES ($1, $2) \
         ON CONFLICT (name) DO NOTHING",
        &[json!(format!("{MAPPING_PREFIX}{legacy_id}")), json!(new_id)],
    )?;
    Ok(())
}

/// Create one configuration Item through the item host, returning its new id.
fn create_config_item(
    item_type: &str,
    title: &str,
    enabled: bool,
    fields: Value,
) -> CoreResult<String> {
    let title = if title.trim().is_empty() {
        "Untitled"
    } else {
        title.trim()
    };
    let saved = item_host::save_item(&json!({
        "type": item_type,
        "title": title,
        "status": i64::from(enabled),
        "fields": fields,
    }))
    .map_err(map_item_err)?;
    saved
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CoreError::Store(format!("save-item returned no id for {item_type}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn an_item_of_the_wrong_type_is_not_a_feed() {
        let item = json!({ "id": "x", "type": "blog", "status": 1, "fields": {} });
        assert!(enabled_item(&item, config::FEED_TYPE).is_none());
    }

    #[test]
    fn an_unpublished_feed_is_not_enabled() {
        let item = json!({ "id": "x", "type": config::FEED_TYPE, "status": 0, "fields": {} });
        assert!(enabled_item(&item, config::FEED_TYPE).is_none());
        assert!(
            typed_item(&item, config::FEED_TYPE).is_some(),
            "still the right type, just paused"
        );
    }

    #[test]
    fn a_published_feed_of_the_right_type_passes_both_gates() {
        let item = json!({ "id": "x", "type": config::FEED_TYPE, "status": 1, "fields": {} });
        assert!(enabled_item(&item, config::FEED_TYPE).is_some());
    }

    #[test]
    fn a_get_item_miss_is_not_an_item() {
        assert!(enabled_item(&Value::Null, config::FEED_TYPE).is_none());
        assert!(typed_item(&Value::Null, config::TOPIC_TYPE).is_none());
    }

    #[test]
    fn an_empty_topic_id_resolves_to_no_topic_without_a_host_call() {
        assert_eq!(load_topic_config("").unwrap(), None);
    }
}
