-- Observable per-item embedding state (P11f / D-51).
--
-- The async-by-default embed path (D-51 reverses PF-4 sub-decision 3's
-- synchronous best-effort) replaces the old silently-swallowed embed failures
-- with an *observable* per-item state: `pending` (enqueued, not yet embedded),
-- `indexed` (an embedding for the active model has landed), or `failed` (the
-- embed job exhausted its retries and dead-lettered). Admin surfaces read this
-- table to answer "is this item findable-by-similarity yet, and if not why?".
--
-- Unlike `item_embeddings` (pgvector-gated — created only when the extension is
-- present), this table is plain: it exists everywhere so the embedding
-- lifecycle is observable and backfill can find gaps even in deployments
-- without pgvector installed.

CREATE TABLE IF NOT EXISTS item_embed_status (
    -- One row per item; the whole-item embedding is one logical unit.
    item_id      UUID PRIMARY KEY REFERENCES item(id) ON DELETE CASCADE,

    -- Lifecycle state. `pending` on enqueue, `indexed` on a successful embed,
    -- `failed` when the embed job dead-letters.
    state        TEXT NOT NULL DEFAULT 'pending'
                 CHECK (state IN ('pending', 'indexed', 'failed')),

    -- The embedding model that produced the current `indexed` embedding
    -- (NULL while pending / on failure). Backfill compares this to the active
    -- model so a model change re-enqueues stale items.
    model        VARCHAR(128),

    -- Hash of the embeddable text that was (or is being) indexed. Lets the
    -- drain detect a superseded job (content changed since enqueue) and skip
    -- re-embedding unchanged content.
    content_hash TEXT,

    -- Unix timestamp when the current embedding landed (NULL until indexed).
    embedded_at  BIGINT,

    -- Last embed error, preserved when `state = 'failed'` for admin inspection.
    last_error   TEXT,

    -- Unix timestamp of the last state change.
    updated_at   BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM now())::BIGINT
);

-- Backfill scans for items whose state is not `indexed` for the active model;
-- this index covers that gap query.
CREATE INDEX IF NOT EXISTS idx_item_embed_status_state
    ON item_embed_status (state);
