-- Create item_type table for content type definitions
-- Story 4.1: Item Type Table and Schema

CREATE TABLE item_type (
    -- Machine name (e.g., "blog", "page")
    type VARCHAR(32) PRIMARY KEY,

    -- Human-readable label
    label VARCHAR(255) NOT NULL,

    -- Description for admin UI
    description TEXT,

    -- Whether items of this type have a title field
    has_title BOOLEAN NOT NULL DEFAULT true,

    -- Custom label for the title field
    title_label VARCHAR(255) DEFAULT 'Title',

    -- Plugin that defines this content type
    plugin VARCHAR(64) NOT NULL,

    -- Field definitions and other type settings
    settings JSONB DEFAULT '{}'::jsonb
);

-- Index for plugin lookups
CREATE INDEX idx_item_type_plugin ON item_type(plugin);

-- Seed default "page" content type
INSERT INTO item_type (type, label, description, plugin, settings)
VALUES (
    'page',
    'Basic Page',
    'A simple page with a title and body.',
    'core',
    '{"fields": [{"field_name": "body", "field_type": {"TextLong": null}, "label": "Body", "required": false, "cardinality": 1}]}'::jsonb
);
