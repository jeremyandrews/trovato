//! The static search index's change queue (`search_index_queue`).
//!
//! Every write that can change what the public Pagefind index holds queues
//! the items it touched here; cron drains the queue and rewrites only those
//! items' part of the index ([`crate::cron`]'s search index update, designed in
//! `docs/design/incremental-search-index.md`). Before this, every such write
//! set one "rebuild everything" flag.
//!
//! Writes to the rows an item's page is built from are queued by the database
//! itself. Triggers on `item`, `comment` and `url_alias` (migration
//! `20261204000001_search_index_queue_triggers.sql`) queue the item in the
//! same transaction as the write, whoever made it: a kernel service, a bulk
//! statement or a plugin's raw SQL. A write that commits therefore cannot lose
//! its queue row to a crash or an error after the commit, and one that rolls
//! back queues nothing.
//!
//! What no trigger sees is queued here by the code that changes it: a type's
//! search settings or field weights ([`enqueue_type`]), workspace publishing
//! ([`enqueue_stage_publish`], which also names the reason its moves are
//! queued under), and a plugin's own data shown in an item's page, through the
//! `db.search-index-enqueue` host function
//! (`trovato_sdk::host::search_index_enqueue`).
//!
//! Queuing is cheap and idempotent: one row per item, replaced by the next
//! change, so a burst of writes to one item costs one update. An item queued
//! that is not, or is no longer, searchable is taken out of the index; one
//! queued that changed nothing the index shows is a no-op. When in doubt,
//! queue.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use sqlx::PgExecutor;

use uuid::Uuid;

use crate::models::stage::LIVE_STAGE_ID;

/// Most item ids a plugin may queue in one host call.
pub const MAX_ENQUEUE: usize = 10_000;

/// What a queued item asks for. The drain decides searchability from the item
/// itself, so `Upsert` on an item that is no longer searchable removes it; the
/// op says what the writer knew, and is kept for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOp {
    /// The item was written: index it as it is now.
    Upsert,
    /// The item was deleted: take it out.
    Delete,
}

impl IndexOp {
    /// The `op` column value.
    pub fn as_i16(self) -> i16 {
        match self {
            Self::Upsert => 1,
            Self::Delete => 2,
        }
    }
}

/// One item to queue: its id, its language if the writer knows it, and the op.
#[derive(Debug, Clone)]
pub struct Queued {
    /// The item.
    pub item_id: Uuid,
    /// Its language, for the log; the drain reads the item.
    pub language: Option<String>,
    /// The op.
    pub op: IndexOp,
}

impl Queued {
    /// An upsert of `item_id`.
    pub fn upsert(item_id: Uuid) -> Self {
        Self {
            item_id,
            language: None,
            op: IndexOp::Upsert,
        }
    }
}

/// Queue `items` for the static search index, `reason` saying what wrote them.
/// An item already queued is replaced (its op, reason and sequence number),
/// so it is applied once, from its state at drain time. Returns how many rows
/// were written.
pub async fn enqueue<'e, E: PgExecutor<'e>>(
    executor: E,
    items: &[Queued],
    reason: &str,
) -> Result<u64> {
    // Last op per item wins; one statement may not touch a row twice.
    let mut by_item: BTreeMap<Uuid, &Queued> = BTreeMap::new();
    for item in items {
        by_item.insert(item.item_id, item);
    }
    let chunk: Vec<&Queued> = by_item.into_values().collect();
    if chunk.is_empty() {
        return Ok(0);
    }
    {
        let ids: Vec<Uuid> = chunk.iter().map(|q| q.item_id).collect();
        let languages: Vec<Option<String>> = chunk.iter().map(|q| q.language.clone()).collect();
        let ops: Vec<i16> = chunk.iter().map(|q| q.op.as_i16()).collect();
        let result = sqlx::query(
            "INSERT INTO search_index_queue (item_id, language, op, reason) \
             SELECT u.item_id, u.language, u.op, $4 \
             FROM UNNEST($1::uuid[], $2::text[], $3::smallint[]) AS u(item_id, language, op) \
             ON CONFLICT (item_id) DO UPDATE SET \
                 language = EXCLUDED.language, op = EXCLUDED.op, reason = EXCLUDED.reason, \
                 seq = EXCLUDED.seq, enqueued_at = EXCLUDED.enqueued_at",
        )
        .bind(&ids)
        .bind(&languages)
        .bind(&ops)
        .bind(reason)
        .execute(executor)
        .await
        .context("queue items for the search index")?;
        Ok(result.rows_affected())
    }
}

/// Queue every item of `item_type` that the index holds or may hold: those
/// published on the live stage, and those with a page in the ledger. For a
/// change to the type's search switches or field weights, which can change
/// every one of its pages, or take them all out. One statement.
pub async fn enqueue_type<'e, E: PgExecutor<'e>>(
    executor: E,
    item_type: &str,
    reason: &str,
) -> Result<u64> {
    let result = sqlx::query(
        "INSERT INTO search_index_queue (item_id, language, op, reason) \
         SELECT i.id, i.language, 1, $3 FROM item i \
         WHERE i.type = $1 \
           AND ((i.status = 1 AND i.stage_id = $2) \
                OR EXISTS (SELECT 1 FROM search_page p WHERE p.item_id = i.id)) \
         ON CONFLICT (item_id) DO UPDATE SET \
             language = EXCLUDED.language, op = EXCLUDED.op, reason = EXCLUDED.reason, \
             seq = EXCLUDED.seq, enqueued_at = EXCLUDED.enqueued_at",
    )
    .bind(item_type)
    .bind(LIVE_STAGE_ID)
    .bind(reason)
    .execute(executor)
    .await
    .context("queue a content type for the search index")?;
    Ok(result.rows_affected())
}

/// How many items are queued.
pub async fn pending<'e, E: PgExecutor<'e>>(executor: E) -> Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM search_index_queue")
        .fetch_one(executor)
        .await
        .context("count the search index queue")
}

/// Queue everything publishing workspace `stage_id` to Live changes in the
/// static index, inside the publish's own transaction (so the queue rows
/// commit with the move): every item on the stage, every item the publish
/// deletes, and every item whose URL alias moves or is deleted with it.
/// Call before the items and aliases leave the stage.
///
/// It also sets the transaction's `trovato.search_index_reason` to
/// `stage_publish`, so the rows the item, comment and alias triggers queue as
/// the publish moves them carry that reason too.
pub async fn enqueue_stage_publish(conn: &mut sqlx::PgConnection, stage_id: Uuid) -> Result<u64> {
    sqlx::query("SELECT set_config('trovato.search_index_reason', 'stage_publish', true)")
        .execute(&mut *conn)
        .await
        .context("name the workspace publish's search index reason")?;
    let result = sqlx::query(
        r#"
        WITH deleted AS (
            SELECT entity_id::uuid AS id FROM stage_deletion
            WHERE stage_id = $1 AND entity_type = 'item'
        ),
        aliased AS (
            SELECT substring(a.source FROM 7)::uuid AS id FROM url_alias a
            -- A whole UUID: a looser pattern would let a malformed source
            -- reach the cast, and its error would abort the publish.
            WHERE a.source ~ '^/item/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
              AND (a.stage_id = $1 OR a.id IN (
                  SELECT entity_id::uuid FROM stage_deletion
                  WHERE stage_id = $1 AND entity_type = 'url_alias'))
        ),
        touched AS (
            SELECT id, 1::smallint AS op FROM item WHERE stage_id = $1
            UNION SELECT id, 1::smallint FROM aliased
            UNION SELECT id, 2::smallint FROM deleted
        )
        INSERT INTO search_index_queue (item_id, language, op, reason)
        SELECT DISTINCT ON (t.id) t.id, i.language, t.op, 'stage_publish'
        FROM touched t LEFT JOIN item i ON i.id = t.id
        ORDER BY t.id, t.op DESC
        ON CONFLICT (item_id) DO UPDATE SET
            language = EXCLUDED.language, op = EXCLUDED.op, reason = EXCLUDED.reason,
            seq = EXCLUDED.seq, enqueued_at = EXCLUDED.enqueued_at
        "#,
    )
    .bind(stage_id)
    .execute(conn)
    .await
    .context("queue a workspace publish for the search index")?;
    Ok(result.rows_affected())
}
