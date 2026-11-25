-- B218: the incremental search index's change queue and word chunk plan
-- (docs/design/incremental-search-index.md, part 3 of 3). Expand only: two new
-- tables and a sequence, nothing an older release reads.

-- Items whose part of the search index may have changed, one row per item:
-- a second change to an item before cron drains it replaces its row (and its
-- `seq`), so the drain applies the item once, from its current state. The
-- drain deletes exactly the (item_id, seq) pairs it read, so a change queued
-- while a drain runs stays for the next one.
CREATE SEQUENCE search_index_queue_seq;

CREATE TABLE search_index_queue (
    item_id     UUID        PRIMARY KEY,
    -- The item's language when it was queued; the drain reads the item itself.
    language    TEXT        NULL,
    -- 1 upsert, 2 delete. An upsert of an item that is no longer searchable
    -- is a delete; the drain decides through the same seam as a full build.
    op          SMALLINT    NOT NULL CHECK (op IN (1, 2)),
    -- What queued it: item, comment, type_settings, field_config,
    -- reindex, stage_publish, url_alias, plugin:<plugin name>.
    reason      TEXT        NOT NULL,
    seq         BIGINT      NOT NULL DEFAULT nextval('search_index_queue_seq'),
    enqueued_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX search_index_queue_seq_idx ON search_index_queue (seq);

-- Each language's word chunks as last published: the real first and last
-- word (the meta file holds only distinguishing prefixes), the size by
-- Pagefind's 20,000 location rule, and the file stem. Cut by the last full
-- build and kept frozen by the updater, which routes new words by it.
CREATE TABLE search_chunk (
    language   TEXT    NOT NULL,
    ordinal    INTEGER NOT NULL CHECK (ordinal >= 0),
    first_word TEXT    NOT NULL,
    last_word  TEXT    NOT NULL,
    locations  BIGINT  NOT NULL,
    hash       TEXT    NOT NULL,
    PRIMARY KEY (language, ordinal)
);
