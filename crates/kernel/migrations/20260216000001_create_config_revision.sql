-- Create config_revision table for versioned config entity snapshots
-- Story 21.2: Config Revision Schema (v1.0 Schema Only)
--
-- PURPOSE: This table stores versioned snapshots of config entities.
-- In v1.0, this table remains EMPTY - it's scaffolding for post-MVP
-- stage-aware config storage.
--
-- Post-MVP Usage:
-- - StageAwareConfigStorage writes here when saving config in a stage
-- - Each revision captures the full entity state as JSONB
-- - config_stage_association links stages to specific revisions
-- - On publish, the staged revision becomes the live version

CREATE TABLE config_revision (
    -- UUIDv7 for time-sortable revision IDs
    id UUID PRIMARY KEY,

    -- Config entity type (matches ConfigEntity.entity_type())
    -- Values: item_type, search_field_config, category, tag, variable
    entity_type VARCHAR(64) NOT NULL,

    -- Config entity ID (matches ConfigEntity.id())
    -- VARCHAR(255) because IDs can be strings like "page" or UUIDs as strings
    entity_id VARCHAR(255) NOT NULL,

    -- Complete entity state as JSONB snapshot
    -- Contains serialized ConfigEntity data for this revision
    data JSONB NOT NULL,

    -- Unix timestamp when this revision was created
    created BIGINT NOT NULL,

    -- User who created this revision (nullable for system-generated)
    author_id UUID REFERENCES users(id)
);

-- Index for looking up revisions by entity
CREATE INDEX idx_config_revision_entity ON config_revision(entity_type, entity_id);

-- Index for time-ordered queries (most recent first)
CREATE INDEX idx_config_revision_created ON config_revision(created DESC);

-- Composite index for entity + time queries
CREATE INDEX idx_config_revision_entity_created ON config_revision(entity_type, entity_id, created DESC);
