-- B217: the native search index's page ledger, term cache, publish state and
-- file retention (docs/design/incremental-search-index.md, part 2 of 3).
-- Expand only: four new tables, nothing an older release reads.

-- Every searchable item's page number in its language's Pagefind index, for
-- life. A row with no item is a tombstone, a freed page number: the free list
-- is the set of them, and the next new item in the language takes the lowest.
CREATE TABLE search_page (
    -- The Pagefind language key: the item's language, lowercased.
    language    TEXT        NOT NULL,
    -- The page's position in the language's MetaIndex.pages.
    page_number INTEGER     NOT NULL CHECK (page_number >= 0),
    -- NULL: a tombstone.
    item_id     UUID        NULL UNIQUE,
    -- When the slot became a tombstone; NULL while an item holds it.
    freed_at    TIMESTAMPTZ NULL,
    PRIMARY KEY (language, page_number),
    CHECK ((item_id IS NULL) = (freed_at IS NOT NULL))
);

-- What each searchable item contributes to the index, independent of its page
-- number, keyed by the fragment HTML the tokenizer read.
CREATE TABLE search_page_terms (
    item_id      UUID    PRIMARY KEY,
    language     TEXT    NOT NULL,
    -- SHA-256 of the rendered fragment HTML.
    content_hash BYTEA   NOT NULL,
    word_count   INTEGER NOT NULL,
    -- The fragment JSON, verbatim.
    fragment     TEXT    NOT NULL,
    -- CBOR map: word -> locs (i32 array, as encoded).
    words        BYTEA   NOT NULL
);

-- One row: what is published, for the single-publisher check.
CREATE TABLE search_index_state (
    id               BOOLEAN     PRIMARY KEY DEFAULT true CHECK (id),
    -- Bumped by every publish.
    generation       BIGINT      NOT NULL,
    -- The Pagefind release whose format and tokenizer wrote it.
    pagefind_version TEXT        NOT NULL,
    -- Language -> the meta file stem the entry names.
    entry_meta       JSONB       NOT NULL,
    published_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Content-addressed index files the published entry no longer reaches, and
-- since when. Deleted from disk once unreferenced for the retention window
-- (24 hours), so a reader that loaded an older meta keeps finding its files.
CREATE TABLE search_index_file (
    -- Path inside PAGEFIND_OUTPUT_DIR, `/` separated.
    path            TEXT        PRIMARY KEY,
    unreferenced_at TIMESTAMPTZ NOT NULL
);
