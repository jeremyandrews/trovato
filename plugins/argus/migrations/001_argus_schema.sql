-- Argus pipeline schema (M1-2). Forward-only migration; no rollback. Kernel
-- tables are guaranteed to exist. Every table here is plugin-owned: creating
-- them via this migration is what puts them in the plugin's db allowlist, so
-- the pipeline can read/write them through the `db` host (raw_sql = true).
--
-- Split of concerns (ARCHITECTURE.md §9.4):
--   argus_articles  — high-volume pipeline data, declared as a lightweight
--                     record type in argus.info.toml (gather + read-only admin
--                     + reverse reference at plain-table cost, no Item tax).
--   argus_feeds     — operational feed config + mutable fetch state (etag,
--                     failure tracking, last_fetched_at). Plugin-owned table.
--   argus_topics    — relevance criteria per topic. Plugin-owned table.
--   argus_state     — key/value cursor store (the ritrovo_importer pattern);
--                     holds the round-robin scheduling cursor as text.
--
-- argus_story stays a kernel Item (declared in tap_item_info) so it keeps
-- semantic search; it is not created here.

-- ---------------------------------------------------------------------------
-- Articles (lightweight record type)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS argus_articles (
    id               UUID PRIMARY KEY,
    url              TEXT NOT NULL,
    title            VARCHAR(255) NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    published_at     BIGINT,
    feed_id          UUID,
    topic_id         UUID,
    story_id         UUID,
    relevance_score  INTEGER,
    relevance_reason TEXT,
    summary          TEXT,
    analysis         TEXT,
    pipeline_state   TEXT NOT NULL DEFAULT 'fetched',
    pipeline_error   TEXT,
    content_hash     TEXT,
    created          BIGINT NOT NULL,
    changed          BIGINT NOT NULL
);

-- The dedup key: one row per canonical URL. The idempotent upsert targets this
-- constraint so an at-least-once queue replay leaves exactly one row (M1-6).
CREATE UNIQUE INDEX IF NOT EXISTS uniq_argus_articles_url ON argus_articles (url);
CREATE INDEX IF NOT EXISTS idx_argus_articles_topic ON argus_articles (topic_id);
CREATE INDEX IF NOT EXISTS idx_argus_articles_state ON argus_articles (pipeline_state);
-- Reverse RecordReference story -> its articles (M1-2) resolves via this index.
CREATE INDEX IF NOT EXISTS idx_argus_articles_story ON argus_articles (story_id);
-- Near-duplicate lookup across feeds (M1-6).
CREATE INDEX IF NOT EXISTS idx_argus_articles_hash ON argus_articles (content_hash);

-- ---------------------------------------------------------------------------
-- Feeds (operational config + fetch state)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS argus_feeds (
    id                     UUID PRIMARY KEY,
    url                    TEXT NOT NULL,
    name                   VARCHAR(255) NOT NULL DEFAULT '',
    topic_id               UUID NOT NULL,
    fetch_interval_seconds BIGINT NOT NULL DEFAULT 900,
    etag                   TEXT,
    last_modified          TEXT,
    last_fetched_at        BIGINT,
    failure_count          INTEGER NOT NULL DEFAULT 0,
    last_error             TEXT,
    enabled                BOOLEAN NOT NULL DEFAULT true,
    created                BIGINT NOT NULL,
    changed                BIGINT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uniq_argus_feeds_url ON argus_feeds (url);
CREATE INDEX IF NOT EXISTS idx_argus_feeds_enabled ON argus_feeds (enabled);

-- ---------------------------------------------------------------------------
-- Topics (relevance criteria)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS argus_topics (
    id                  UUID PRIMARY KEY,
    name                VARCHAR(255) NOT NULL,
    relevance_prompt    TEXT NOT NULL DEFAULT '',
    relevance_threshold INTEGER NOT NULL DEFAULT 50,
    enabled             BOOLEAN NOT NULL DEFAULT true,
    created             BIGINT NOT NULL,
    changed             BIGINT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uniq_argus_topics_name ON argus_topics (name);

-- ---------------------------------------------------------------------------
-- Cursor / key-value state (the ritrovo_importer pattern)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS argus_state (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
