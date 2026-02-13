-- Search field configuration table
-- Story 12.2: Search Field Configuration

CREATE TABLE search_field_config (
    id UUID PRIMARY KEY,
    -- Content type this config applies to
    bundle VARCHAR(32) NOT NULL REFERENCES item_type(type) ON DELETE CASCADE,
    -- Field name to index (from JSONB fields)
    field_name VARCHAR(64) NOT NULL,
    -- Search weight: A (highest) to D (lowest)
    weight CHAR(1) NOT NULL DEFAULT 'C' CHECK (weight IN ('A', 'B', 'C', 'D')),
    -- Unique constraint per bundle/field
    UNIQUE (bundle, field_name)
);

-- Index for looking up config by bundle
CREATE INDEX idx_search_config_bundle ON search_field_config(bundle);

-- Comment explaining usage
COMMENT ON TABLE search_field_config IS 'Configuration for which fields are indexed in full-text search per content type';
COMMENT ON COLUMN search_field_config.weight IS 'PostgreSQL tsvector weight: A=highest relevance, D=lowest';
