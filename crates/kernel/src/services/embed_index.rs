//! Async auto-embed: kernel-internal producer, policy, observable state, and
//! backfill (P11f / D-51, D-52).
//!
//! Item save no longer embeds inline. Instead it enqueues a kernel-owned embed
//! job on queue v2 (D-52) under a reserved `plugin_name` routed to a native
//! drain arm (see [`crate::cron`]), and records an **observable** per-item
//! embedding state (D-51) in `item_embed_status` — replacing the old
//! silently-swallowed embed failures (PF-4 sub-decision 3, deliberately
//! reversed).
//!
//! This module holds the pieces shared across the three call sites — the save
//! path (`item_service`), the drain ([`crate::cron`]), and the
//! admin backfill ([`crate::routes::admin_embed`]):
//!
//! - the reserved queue identity + native worker width;
//! - the [`EmbedPolicy`] (async-by-default, per-content-type opt-out, D-51);
//! - the content hash used for coalescing / stale-job detection;
//! - the kernel producer ([`enqueue_embed_job`]);
//! - the `item_embed_status` read/write helpers;
//! - the backfill gap query.
//!
//! # Freshness contract (D-51)
//!
//! An item is **findable-by-text immediately** — the `search_vector` `tsvector`
//! is maintained by a DB trigger inside the save transaction, so it is never
//! deferred. An item is **findable-by-similarity only after its embed job
//! runs**; that latency is bounded by the queue drain cadence documented in
//! `docs/plugin-queue.md` (external cron by default; the opt-in resident runner
//! for sub-cron latency). Semantic gather degrades correctly over not-yet-
//! embedded items: they simply are not similarity candidates (they carry no
//! `item_embeddings` row for the active model), never an error.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::SiteConfig;

/// Reserved `plugin_queue.plugin_name` for kernel-owned embed jobs (P11f).
///
/// The leading underscore guarantees no collision with a real plugin: plugin
/// machine names must begin with a lowercase letter
/// ([`crate::routes::helpers::is_valid_machine_name`]), so `__kernel_embed` can
/// never be a registered plugin. The drain routes rows with this name to the
/// native embed handler instead of dispatching `tap_queue_worker`.
pub const KERNEL_EMBED_PLUGIN: &str = "__kernel_embed";

/// Logical queue name for kernel embed jobs (there is only one).
pub const EMBED_QUEUE_NAME: &str = "embed";

/// Native embed-worker width: how many embed jobs the kernel arm dispatches
/// concurrently per drain batch. The kernel-internal arm has no
/// `tap_queue_info` to declare a width, so it uses a fixed bound. Set to the
/// kernel concurrency **ceiling** (`QUEUE_CONCURRENCY_CAP = 4`, P11d / D-47) —
/// the most a plugin worker may ever run at once — so embedding keeps pace
/// without exceeding the established kernel cap.
pub const KERNEL_EMBED_CONCURRENCY: usize = 4;

/// `site_config` key holding the [`EmbedPolicy`] (D-51 async-by-default).
pub const EMBED_POLICY_CONFIG_KEY: &str = "embed_policy";

/// Embedding policy (P11f / D-51): async-by-default with a per-content-type
/// opt-out.
///
/// Async (queue-v2 deferred embed) is the default for **every** content type.
/// A type listed in `sync_types` opts out and keeps the pre-P11f synchronous
/// best-effort embed on the save path (unchanged code). An empty policy — the
/// default, and what an absent `embed_policy` key deserializes to — means every
/// type embeds asynchronously.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedPolicy {
    /// Content types that opt out of async and embed synchronously on save.
    #[serde(default)]
    pub sync_types: Vec<String>,
}

impl EmbedPolicy {
    /// Load the policy from `site_config`. A missing key, or an unparseable
    /// value, yields the default (everything async) so a broken config never
    /// silently disables embedding.
    pub async fn load(pool: &PgPool) -> Self {
        match SiteConfig::get(pool, EMBED_POLICY_CONFIG_KEY).await {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// Whether `item_type` embeds asynchronously (the default) rather than
    /// synchronously on the save path.
    pub fn is_async(&self, item_type: &str) -> bool {
        !self.sync_types.iter().any(|t| t == item_type)
    }
}

/// Payload of a kernel embed job. Carries the item to embed and the content
/// hash captured at enqueue time; the drain compares the latter against the
/// item's current content to coalesce superseded jobs (stale-job detection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedJobPayload {
    /// The item to (re)embed.
    pub item_id: Uuid,
    /// Hash of the embeddable text at enqueue time (see [`embed_content_hash`]).
    pub content_hash: String,
}

/// Stable hash of an item's embeddable text, used for coalescing.
///
/// Two saves that leave the embeddable text unchanged produce the same hash (so
/// a no-op edit does not force a re-embed); any change produces a different one
/// (so the drain can tell a job is superseded). SHA-256 hex — deterministic
/// across processes (unlike `DefaultHasher`), which matters because the hash is
/// persisted in the job payload and compared in a later, possibly different,
/// process.
pub fn embed_content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// The current embeddable-content hash of an item (title + stripped field
/// text), as captured on the save path and re-derived by the drain for
/// coalescing. Convenience over [`embed_content_hash`] +
/// `item_embedding_text`.
pub fn item_content_hash(item: &crate::models::Item) -> String {
    embed_content_hash(&crate::content::item_service::item_embedding_text(item))
}

/// Whether a job whose payload captured `payload_hash` has been superseded by a
/// newer save (the item's current embeddable text now hashes to
/// `current_hash`). A superseded job is coalesced away — the newer save's job
/// carries the current hash and embeds the latest content exactly once.
pub fn is_stale(payload_hash: &str, current_hash: &str) -> bool {
    payload_hash != current_hash
}

/// Enqueue a kernel embed job for `item_id` and mark the item `pending`
/// (P11f / D-52). The producer half of the async path: one cheap INSERT on the
/// save path in place of a blocking provider round-trip.
///
/// The job runs under the background AI principal via the native drain arm and
/// inherits queue-v2 retry/backoff/DLQ.
pub async fn enqueue_embed_job(pool: &PgPool, item_id: Uuid, content_hash: &str) -> Result<()> {
    let payload = serde_json::to_value(EmbedJobPayload {
        item_id,
        content_hash: content_hash.to_string(),
    })
    .context("serialize embed job payload")?;

    crate::host::enqueue_kernel_job(pool, KERNEL_EMBED_PLUGIN, EMBED_QUEUE_NAME, &payload, 0, 0)
        .await
        .context("enqueue embed job")?;

    mark_pending(pool, item_id, content_hash).await
}

/// Record that an item's embedding is queued but not yet landed. Keeps any
/// previously-`indexed` model/timestamp (the old embedding stays findable until
/// the new one lands) while flipping state to `pending` and clearing any prior
/// error.
pub async fn mark_pending(pool: &PgPool, item_id: Uuid, content_hash: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO item_embed_status (item_id, state, content_hash, updated_at)
        VALUES ($1, 'pending', $2, $3)
        ON CONFLICT (item_id) DO UPDATE SET
            state = 'pending',
            content_hash = EXCLUDED.content_hash,
            last_error = NULL,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(item_id)
    .bind(content_hash)
    .bind(now)
    .execute(pool)
    .await
    .context("mark embed status pending")?;
    Ok(())
}

/// Record that an item's embedding for `model` has landed (D-51 `indexed`).
pub async fn mark_indexed(
    pool: &PgPool,
    item_id: Uuid,
    model: &str,
    content_hash: &str,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO item_embed_status
            (item_id, state, model, content_hash, embedded_at, last_error, updated_at)
        VALUES ($1, 'indexed', $2, $3, $4, NULL, $4)
        ON CONFLICT (item_id) DO UPDATE SET
            state = 'indexed',
            model = EXCLUDED.model,
            content_hash = EXCLUDED.content_hash,
            embedded_at = EXCLUDED.embedded_at,
            last_error = NULL,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(item_id)
    .bind(model)
    .bind(content_hash)
    .bind(now)
    .execute(pool)
    .await
    .context("mark embed status indexed")?;
    Ok(())
}

/// Record that an item's embed job terminally failed (dead-lettered, D-51
/// `failed`). Preserves the error for admin inspection. This is the observable
/// replacement for the old silently-swallowed embed failure.
pub async fn mark_failed(pool: &PgPool, item_id: Uuid, error: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO item_embed_status (item_id, state, last_error, updated_at)
        VALUES ($1, 'failed', $2, $3)
        ON CONFLICT (item_id) DO UPDATE SET
            state = 'failed',
            last_error = EXCLUDED.last_error,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(item_id)
    .bind(error)
    .bind(now)
    .execute(pool)
    .await
    .context("mark embed status failed")?;
    Ok(())
}

/// Counts of items in each embedding state, for the admin surface.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EmbedStatusCounts {
    /// Items whose embed job is queued but has not yet landed.
    pub pending: i64,
    /// Items with a current embedding.
    pub indexed: i64,
    /// Items whose embed job dead-lettered.
    pub failed: i64,
}

/// Aggregate `item_embed_status` by state.
pub async fn status_counts(pool: &PgPool) -> Result<EmbedStatusCounts> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT state, COUNT(*) FROM item_embed_status GROUP BY state",
    )
    .fetch_all(pool)
    .await
    .context("aggregate embed status")?;

    let mut counts = EmbedStatusCounts::default();
    for (state, n) in rows {
        match state.as_str() {
            "pending" => counts.pending = n,
            "indexed" => counts.indexed = n,
            "failed" => counts.failed = n,
            _ => {}
        }
    }
    Ok(counts)
}

/// Find up to `limit` items missing an `indexed` embedding for `active_model`
/// (P11f backfill / model-change re-embed).
///
/// An item is a gap if it has no `item_embed_status` row, or its row is not
/// `indexed`, or it is `indexed` under a *different* model (the model-change
/// case). Keyed on `item_embed_status` rather than `item_embeddings` so the
/// query works even where pgvector is absent.
pub async fn find_backfill_gaps(
    pool: &PgPool,
    active_model: &str,
    limit: i64,
) -> Result<Vec<Uuid>> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT i.id
        FROM item i
        LEFT JOIN item_embed_status s ON s.item_id = i.id
        WHERE s.item_id IS NULL
           OR s.state <> 'indexed'
           OR s.model IS DISTINCT FROM $1
        ORDER BY i.changed DESC
        LIMIT $2
        "#,
    )
    .bind(active_model)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("query embed backfill gaps")?;
    Ok(ids)
}

/// Enqueue embed jobs for up to `limit` items missing an `indexed` embedding
/// for `active_model` (P11f admin backfill / model-change re-embed).
///
/// Bounded by `limit` so a large site backfills in admin-triggered batches
/// rather than one unbounded sweep. Each job carries the item's current content
/// hash, so a backfill job coalesces correctly against any concurrent save.
/// Returns the number of jobs enqueued (gaps with empty embeddable text are
/// skipped).
pub async fn enqueue_backfill(pool: &PgPool, active_model: &str, limit: i64) -> Result<usize> {
    let gaps = find_backfill_gaps(pool, active_model, limit).await?;
    let mut enqueued = 0usize;
    for id in gaps {
        let Some(item) = crate::models::Item::find_by_id(pool, id)
            .await
            .context("load item for backfill")?
        else {
            continue;
        };
        let text = crate::content::item_service::item_embedding_text(&item);
        if text.trim().is_empty() {
            continue;
        }
        if enqueue_embed_job(pool, id, &embed_content_hash(&text))
            .await
            .is_ok()
        {
            enqueued += 1;
        }
    }
    Ok(enqueued)
}

/// Load one item's embedding status row (admin inspection). `None` when the
/// item has never been enqueued.
pub async fn embed_status(pool: &PgPool, item_id: Uuid) -> Result<Option<ItemEmbedStatus>> {
    let row = sqlx::query_as::<_, ItemEmbedStatus>(
        r#"
        SELECT item_id, state, model, content_hash, embedded_at, last_error, updated_at
        FROM item_embed_status WHERE item_id = $1
        "#,
    )
    .bind(item_id)
    .fetch_optional(pool)
    .await
    .context("load embed status")?;
    Ok(row)
}

/// One item's embedding lifecycle row.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ItemEmbedStatus {
    /// The item.
    pub item_id: Uuid,
    /// `pending` | `indexed` | `failed`.
    pub state: String,
    /// Model of the current embedding (NULL until indexed).
    pub model: Option<String>,
    /// Hash of the indexed / in-flight embeddable text.
    pub content_hash: Option<String>,
    /// When the current embedding landed (NULL until indexed).
    pub embedded_at: Option<i64>,
    /// Last embed error (set when `state = failed`).
    pub last_error: Option<String>,
    /// Last state change (Unix seconds).
    pub updated_at: i64,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn policy_default_is_all_async() {
        let p = EmbedPolicy::default();
        assert!(p.is_async("article"));
        assert!(p.is_async("page"));
        // A bare `{}` from site_config deserializes to the same all-async default.
        let from_empty: EmbedPolicy = serde_json::from_str("{}").unwrap();
        assert!(from_empty.is_async("anything"));
    }

    #[test]
    fn policy_opt_out_is_sync_for_listed_types_only() {
        let p = EmbedPolicy {
            sync_types: vec!["page".to_string()],
        };
        // Listed type opts out of async (embeds synchronously on save).
        assert!(!p.is_async("page"));
        // Every other type stays async.
        assert!(p.is_async("article"));
        assert!(p.is_async("conference"));
    }

    #[test]
    fn content_hash_is_stable_and_content_sensitive() {
        // Same text → same hash (a no-op edit does not force a re-embed).
        assert_eq!(
            embed_content_hash("hello world"),
            embed_content_hash("hello world")
        );
        // Different text → different hash (a real edit is detectable).
        assert_ne!(
            embed_content_hash("hello world"),
            embed_content_hash("hello worlds")
        );
        // SHA-256 hex is 64 chars.
        assert_eq!(embed_content_hash("x").len(), 64);
    }

    #[test]
    fn stale_detection_flags_superseded_content() {
        let h1 = embed_content_hash("first version");
        let h2 = embed_content_hash("second version");
        // A job whose payload hash no longer matches the current content is stale.
        assert!(is_stale(&h1, &h2));
        // A job whose payload hash matches the current content is fresh.
        assert!(!is_stale(&h2, &h2));
    }

    #[test]
    fn reserved_plugin_name_cannot_collide_with_a_real_plugin() {
        // The reserved kernel queue identity must never match a registrable
        // plugin machine name (which must start with a lowercase letter).
        assert!(!crate::routes::helpers::is_valid_machine_name(
            KERNEL_EMBED_PLUGIN
        ));
        assert_eq!(KERNEL_EMBED_CONCURRENCY, 4);
    }
}
