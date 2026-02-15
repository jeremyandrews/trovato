-- Create config_stage_association table linking stages to config revisions
-- Story 21.2: Config Revision Schema (v1.0 Schema Only)
--
-- PURPOSE: This junction table associates stages with specific config revisions.
-- In v1.0, this table remains EMPTY - it's scaffolding for post-MVP
-- stage-aware config storage.
--
-- Post-MVP Usage:
-- - When a config entity is modified in a stage, an entry is created here
-- - Links (stage_id, entity_type, entity_id) → target_revision_id
-- - Publishing a stage applies all associated revisions to live
-- - Conflict detection uses this table to find competing changes
--
-- NOTE: No FK to stage table because it doesn't exist yet.
-- The item table uses stage_id VARCHAR(64) with default 'live'.
-- A proper stage management table will be added in Story 21.3+.

CREATE TABLE config_stage_association (
    -- Stage identifier (matches item.stage_id pattern)
    -- Default stage is 'live', other stages are user-created
    stage_id VARCHAR(64) NOT NULL,

    -- Config entity type (matches config_revision.entity_type)
    entity_type VARCHAR(64) NOT NULL,

    -- Config entity ID (matches config_revision.entity_id)
    entity_id VARCHAR(255) NOT NULL,

    -- The specific revision of this entity in this stage
    target_revision_id UUID NOT NULL REFERENCES config_revision(id) ON DELETE CASCADE,

    -- Composite primary key: one revision per entity per stage
    PRIMARY KEY (stage_id, entity_type, entity_id)
);

-- Index for finding all staged config by stage
CREATE INDEX idx_config_stage_assoc_stage ON config_stage_association(stage_id);

-- Index for finding all stages that modified a specific entity
CREATE INDEX idx_config_stage_assoc_entity ON config_stage_association(entity_type, entity_id);
