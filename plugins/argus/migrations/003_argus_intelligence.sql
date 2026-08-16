-- Argus milestone 2 (intelligence stages) schema. Forward-only; no rollback.
-- Additive only: every change to an M1 table is an `ADD COLUMN IF NOT EXISTS`,
-- and no M1 column, index or constraint is dropped or redefined.
--
-- Design decisions this schema encodes are argued in M2-DESIGN.md:
--   * No pgvector. The extension is optional (the kernel's own embedding
--     migration is conditional on it, and stock postgres:17 does not ship it),
--     and there is no plugin-facing embedding call to produce a vector for it
--     anyway. Article vectors are deterministic lexical vectors computed in
--     argus-core and stored as a JSON float array in TEXT.
--   * Entities are plugin-owned tables, not Items: high write churn, no
--     per-entity page, and Item writes carry the full Item tax.
--   * `argus_stories.id` IS the `argus_story` Item id. One identifier, so the
--     M1 reverse reference (a gather over articles filtered by `story_id`)
--     keeps working unchanged from the story Item's page.

-- ---------------------------------------------------------------------------
-- Articles: analysis output, duplicate marking, clustering bookkeeping
-- ---------------------------------------------------------------------------
-- `analysis` (M1) keeps the raw model JSON for debugging; the three analysis
-- prose fields below are what the reader UI (A5) will render.
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS critical_analysis TEXT;
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS fallacy_analysis  TEXT;
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS source_analysis   TEXT;
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS analyzed_at       BIGINT;

-- Near-duplicate marking (a re-report of the same piece from another feed).
-- The duplicate stays in its story's source list but is not counted toward the
-- story's article_count.
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS is_duplicate BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS duplicate_of UUID;

-- How many times the cluster stage has deferred this article ("wait
-- unassigned"). Bounds the wait: past ARGUS_MAX_CLUSTER_WAITS the decision is
-- forced to create a story rather than deferring forever.
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS cluster_attempts INTEGER NOT NULL DEFAULT 0;

-- Retention: when the body text was nulled out. Metadata and scores are kept
-- forever; only `content` is reclaimed.
ALTER TABLE argus_articles ADD COLUMN IF NOT EXISTS content_purged_at BIGINT;

-- Clustering pulls candidates by (topic, publication window) and the retention
-- pass sweeps by (state, published_at); both want this composite.
CREATE INDEX IF NOT EXISTS idx_argus_articles_topic_published
    ON argus_articles (topic_id, published_at);

-- ---------------------------------------------------------------------------
-- Entities
-- ---------------------------------------------------------------------------
-- `match_key` is the normalized form used for candidate lookup and fuzzy alias
-- resolution (lowercased, punctuation stripped, whitespace collapsed, leading
-- article dropped). `canonical_name` is the display form of the first spelling
-- seen; later spellings that resolve to this row are appended to `aliases`.
CREATE TABLE IF NOT EXISTS argus_entities (
    id             UUID PRIMARY KEY,
    canonical_name TEXT NOT NULL,
    match_key      TEXT NOT NULL,
    entity_type    TEXT NOT NULL,
    aliases        JSONB NOT NULL DEFAULT '[]'::jsonb,
    article_count  INTEGER NOT NULL DEFAULT 0,
    first_seen_at  BIGINT NOT NULL,
    last_seen_at   BIGINT NOT NULL,
    created        BIGINT NOT NULL,
    changed        BIGINT NOT NULL
);

-- The upsert target: one row per (normalized name, type). Two entities may
-- share a name across types ("Apple" the company and "Apple" the place).
CREATE UNIQUE INDEX IF NOT EXISTS uniq_argus_entities_key_type
    ON argus_entities (match_key, entity_type);
CREATE INDEX IF NOT EXISTS idx_argus_entities_type ON argus_entities (entity_type);
-- Fuzzy candidate lookup narrows by a short match_key prefix before scoring.
CREATE INDEX IF NOT EXISTS idx_argus_entities_prefix
    ON argus_entities (left(match_key, 4));

CREATE TABLE IF NOT EXISTS argus_article_entities (
    article_id UUID NOT NULL,
    entity_id  UUID NOT NULL,
    created    BIGINT NOT NULL,
    PRIMARY KEY (article_id, entity_id)
);
CREATE INDEX IF NOT EXISTS idx_argus_article_entities_entity
    ON argus_article_entities (entity_id);

-- ---------------------------------------------------------------------------
-- Article vectors
-- ---------------------------------------------------------------------------
-- `recipe` records how the vector was produced (e.g. `lex-v1/256`). Changing
-- the recipe or the dimension means the stored vectors are no longer
-- comparable, so the cluster stage skips vectors whose recipe differs from the
-- configured one and the operator re-embeds. `vector` is a JSON float array;
-- see the pgvector note at the top of this file.
CREATE TABLE IF NOT EXISTS argus_article_vectors (
    article_id UUID PRIMARY KEY,
    dim        INTEGER NOT NULL,
    recipe     TEXT NOT NULL,
    vector     TEXT NOT NULL,
    created    BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_argus_article_vectors_recipe
    ON argus_article_vectors (recipe);

-- ---------------------------------------------------------------------------
-- Stories (operational half; the narrative half is the argus_story Item)
-- ---------------------------------------------------------------------------
-- `id` IS the Item id (see the header note). The Item holds title, summary,
-- sources and relevance; this row holds the clustering state that would be
-- expensive to keep in Item fields — the running centroid above all, which is
-- rewritten on every join.
-- `title`, `summary` and `sources` are duplicated here rather than living only
-- on the Item because the `item-api` host has no partial field update:
-- `save-item` replaces the whole `fields` object (`Item::update` does
-- `input.fields.unwrap_or(current.fields)`), so a story mutation that omitted a
-- field would erase it. Keeping the narrative in the row makes the Item a
-- complete projection this plugin can always rewrite from one place, instead of
-- a document it has to read back and merge. See M2-FRICTION G-ITEM-NO-MERGE.
CREATE TABLE IF NOT EXISTS argus_stories (
    id                 UUID PRIMARY KEY,
    topic_id           UUID,
    centroid           TEXT NOT NULL,
    dim                INTEGER NOT NULL,
    recipe             TEXT NOT NULL,
    title              TEXT,
    summary            TEXT,
    sources            TEXT,
    article_count      INTEGER NOT NULL DEFAULT 0,
    first_article_at   BIGINT,
    last_article_at    BIGINT,
    is_active          BOOLEAN NOT NULL DEFAULT true,
    relevance_score    INTEGER,
    summarize_pending  BOOLEAN NOT NULL DEFAULT false,
    last_summarized_at BIGINT,
    created            BIGINT NOT NULL,
    changed            BIGINT NOT NULL
);
-- Candidate selection: active stories in a topic, most recent first.
CREATE INDEX IF NOT EXISTS idx_argus_stories_topic_active
    ON argus_stories (topic_id, is_active, last_article_at);

-- ---------------------------------------------------------------------------
-- Cost accounting
-- ---------------------------------------------------------------------------
-- One row per (UTC day, stage). `unpriced_calls` is tracked separately from
-- `calls` because the host reports `cost_estimate = NULL` for an unpriced
-- model, which is "unknown", not "free": an operator reading a low `cost_usd`
-- needs to see whether it means cheap or unpriced.
CREATE TABLE IF NOT EXISTS argus_cost_daily (
    day            TEXT NOT NULL,
    stage          TEXT NOT NULL,
    calls          INTEGER NOT NULL DEFAULT 0,
    unpriced_calls INTEGER NOT NULL DEFAULT 0,
    cost_usd       DOUBLE PRECISION NOT NULL DEFAULT 0,
    PRIMARY KEY (day, stage)
);
