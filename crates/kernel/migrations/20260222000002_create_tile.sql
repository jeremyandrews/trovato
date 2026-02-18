-- Tile (block) subsystem for placing content in page regions.
CREATE TABLE IF NOT EXISTS tile (
    id UUID PRIMARY KEY,
    -- Machine name for referencing (unique per stage).
    machine_name VARCHAR(128) NOT NULL,
    -- Human-readable label.
    label VARCHAR(255) NOT NULL,
    -- Region on the page (e.g., "sidebar", "header", "footer").
    region VARCHAR(64) NOT NULL DEFAULT 'sidebar',
    -- Tile type: custom_html, menu, gather_query.
    tile_type VARCHAR(64) NOT NULL DEFAULT 'custom_html',
    -- Type-specific configuration (JSON).
    config JSONB NOT NULL DEFAULT '{}',
    -- Visibility rules (JSON): paths, roles, etc.
    visibility JSONB NOT NULL DEFAULT '{}',
    -- Sort weight within a region (lower = higher).
    weight INTEGER NOT NULL DEFAULT 0,
    -- Whether the tile is active.
    status INTEGER NOT NULL DEFAULT 1,
    -- Plugin that owns this tile (or "core").
    plugin VARCHAR(128) NOT NULL DEFAULT 'core',
    -- Stage ID for publish workflows.
    stage_id VARCHAR(128) NOT NULL DEFAULT 'live',
    -- Timestamps.
    created BIGINT NOT NULL DEFAULT 0,
    changed BIGINT NOT NULL DEFAULT 0
);

-- Index for efficient region-based tile loading.
CREATE INDEX IF NOT EXISTS idx_tile_region_stage_weight
    ON tile (region, stage_id, weight);

-- Unique constraint on machine_name per stage.
CREATE UNIQUE INDEX IF NOT EXISTS idx_tile_machine_name_stage
    ON tile (machine_name, stage_id);
