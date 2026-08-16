-- Create stage_deletion table for tracking deletions within stages
-- Story 21.2: Config Revision Schema (v1.0 Schema Only)
--
-- PURPOSE: Tracks entities that have been deleted within a stage but not yet
-- published. When a stage is published, these deletions are applied to live.
-- In v1.0, this table remains EMPTY - it's scaffolding for post-MVP staging.
--
-- Post-MVP Usage:
-- - When an entity is deleted in a stage, record it here (instead of actually deleting)
-- - Stage-aware queries exclude entities listed here for their stage
-- - Publishing a stage executes the actual deletions
-- - Supports both content items and config entities via entity_type
--
-- Entity Types:
-- - 'item' for content items (existing staging support)
-- - 'item_type', 'category', 'tag', 'variable', 'search_field_config' for config entities

CREATE TABLE stage_deletion (
    -- Stage where the deletion occurred
    stage_id VARCHAR(64) NOT NULL,

    -- Type of entity being deleted
    -- 'item' for content, or config entity types from ConfigEntity.entity_type()
    entity_type VARCHAR(64) NOT NULL,

    -- ID of the deleted entity
    -- For items: UUID as string
    -- For config: ConfigEntity.id() which may be string or UUID
    entity_id VARCHAR(255) NOT NULL,

    -- When the deletion was recorded
    deleted_at BIGINT NOT NULL,

    -- User who performed the deletion
    deleted_by UUID REFERENCES users(id),

    -- Composite primary key: one deletion record per entity per stage
    PRIMARY KEY (stage_id, entity_type, entity_id)
);

-- Index for finding all deletions in a stage
CREATE INDEX idx_stage_deletion_stage ON stage_deletion(stage_id);

-- Index for checking if a specific entity is deleted in any stage
CREATE INDEX idx_stage_deletion_entity ON stage_deletion(entity_type, entity_id);
