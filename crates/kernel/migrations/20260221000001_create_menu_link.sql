-- Menu link table for stage-aware menu items.
CREATE TABLE IF NOT EXISTS menu_link (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    menu_name VARCHAR(64) NOT NULL DEFAULT 'main',
    path VARCHAR(512) NOT NULL,
    title VARCHAR(255) NOT NULL,
    parent_id UUID REFERENCES menu_link(id) ON DELETE SET NULL,
    weight INTEGER NOT NULL DEFAULT 0,
    hidden BOOLEAN NOT NULL DEFAULT false,
    plugin VARCHAR(64) NOT NULL DEFAULT 'core',
    stage_id VARCHAR(64) NOT NULL DEFAULT 'live',
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL,
    UNIQUE (path, menu_name, stage_id)
);

CREATE INDEX idx_menu_link_menu_stage ON menu_link(menu_name, stage_id);
CREATE INDEX idx_menu_link_parent ON menu_link(parent_id);
