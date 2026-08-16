-- Category Tag Hierarchy table (junction table for DAG structure)
-- Supports multiple parents per tag for flexible hierarchical taxonomies

CREATE TABLE category_tag_hierarchy (
    id SERIAL PRIMARY KEY,
    tag_id UUID NOT NULL REFERENCES category_tag(id) ON DELETE CASCADE,
    parent_id UUID REFERENCES category_tag(id) ON DELETE CASCADE
);

-- Unique constraint: a tag can only have each parent once
CREATE UNIQUE INDEX idx_tag_hierarchy_unique ON category_tag_hierarchy(tag_id, parent_id) WHERE parent_id IS NOT NULL;

-- Unique constraint: a tag can only have one NULL parent entry (root status)
CREATE UNIQUE INDEX idx_tag_hierarchy_root_unique ON category_tag_hierarchy(tag_id) WHERE parent_id IS NULL;

-- Index for finding children of a parent
CREATE INDEX idx_tag_hierarchy_parent ON category_tag_hierarchy(parent_id);

-- Index for root tags (NULL parent)
CREATE INDEX idx_tag_hierarchy_root ON category_tag_hierarchy(tag_id) WHERE parent_id IS NULL;
