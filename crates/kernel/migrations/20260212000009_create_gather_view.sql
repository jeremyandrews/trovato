-- Gather View table
-- Stores reusable view definitions for the Gather query engine

CREATE TABLE gather_view (
    view_id VARCHAR(64) PRIMARY KEY,
    label VARCHAR(255) NOT NULL,
    description TEXT,
    definition JSONB NOT NULL,  -- ViewDefinition: base_table, filters, sorts, fields, relationships
    display JSONB NOT NULL,      -- ViewDisplay: format, items_per_page, pager config
    plugin VARCHAR(64) NOT NULL,
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL
);

CREATE INDEX idx_view_plugin ON gather_view(plugin);
