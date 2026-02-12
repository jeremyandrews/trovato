-- Create item and item_revision tables for content storage
-- Story 4.2: Item Table with JSONB Fields

-- Main item table
CREATE TABLE item (
    -- UUIDv7 for time-sortable IDs
    id UUID PRIMARY KEY,

    -- Current revision (null for new items before first save)
    current_revision_id UUID,

    -- Content type reference
    type VARCHAR(32) NOT NULL REFERENCES item_type(type),

    -- Item title
    title VARCHAR(255) NOT NULL,

    -- Author user ID
    author_id UUID NOT NULL REFERENCES users(id),

    -- Publication status (0 = unpublished, 1 = published)
    status SMALLINT NOT NULL DEFAULT 1,

    -- Unix timestamps
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL,

    -- Promotion flags for front page, sticky lists
    promote SMALLINT NOT NULL DEFAULT 0,
    sticky SMALLINT NOT NULL DEFAULT 0,

    -- Dynamic field storage (JSONB)
    fields JSONB DEFAULT '{}'::jsonb,

    -- Full-text search vector
    search_vector tsvector,

    -- Stage for content staging (default 'live')
    stage_id VARCHAR(64) NOT NULL DEFAULT 'live'
);

-- Revision history table
CREATE TABLE item_revision (
    -- UUIDv7 for time-sortable IDs
    id UUID PRIMARY KEY,

    -- Parent item
    item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,

    -- Who created this revision
    author_id UUID NOT NULL REFERENCES users(id),

    -- Revision content (snapshot)
    title VARCHAR(255) NOT NULL,
    status SMALLINT NOT NULL DEFAULT 1,
    fields JSONB DEFAULT '{}'::jsonb,

    -- When this revision was created
    created BIGINT NOT NULL,

    -- Revision log message
    log TEXT
);

-- Indexes for item table
CREATE INDEX idx_item_type ON item(type);
CREATE INDEX idx_item_author ON item(author_id);
CREATE INDEX idx_item_status ON item(status);
CREATE INDEX idx_item_created ON item(created);
CREATE INDEX idx_item_changed ON item(changed);
CREATE INDEX idx_item_stage ON item(stage_id);

-- GIN index for JSONB field queries
CREATE INDEX idx_item_fields ON item USING GIN (fields);

-- GIN index for full-text search
CREATE INDEX idx_item_search ON item USING GIN (search_vector);

-- Indexes for revision table
CREATE INDEX idx_revision_item ON item_revision(item_id);
CREATE INDEX idx_revision_item_created ON item_revision(item_id, created DESC);

-- Add FK constraint for current_revision_id (after revision table exists)
ALTER TABLE item ADD CONSTRAINT fk_item_current_revision
    FOREIGN KEY (current_revision_id) REFERENCES item_revision(id);
