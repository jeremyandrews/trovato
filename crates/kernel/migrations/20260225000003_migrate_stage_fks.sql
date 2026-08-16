-- Stage Architecture v2: Migrate all stage_id columns from VARCHAR to UUID.
--
-- Converts stage_id in: item, url_alias, menu_link, tile,
-- config_stage_association, stage_deletion.
--
-- Pattern per table:
-- 1. Add new UUID column
-- 2. Backfill from old VARCHAR via stage_config.machine_name lookup
-- 3. Set NOT NULL + default
-- 4. Drop old column
-- 5. Rename new column
-- 6. Add FK (ON DELETE RESTRICT) + index
--
-- NOTE: This migration is destructive (drops and renames columns).
-- No automated rollback migration exists. Test against a database backup
-- before running in production.

-- ============================================================
-- item
-- ============================================================
ALTER TABLE item ADD COLUMN stage_tag_id UUID;

UPDATE item SET stage_tag_id = (
    SELECT sc.tag_id FROM stage_config sc WHERE sc.machine_name = item.stage_id
);
-- Any items with unknown stage_id default to live
UPDATE item SET stage_tag_id = '0193a5a0-0000-7000-8000-000000000001'::uuid
WHERE stage_tag_id IS NULL;

ALTER TABLE item ALTER COLUMN stage_tag_id SET NOT NULL;
ALTER TABLE item ALTER COLUMN stage_tag_id SET DEFAULT '0193a5a0-0000-7000-8000-000000000001'::uuid;

DROP INDEX IF EXISTS idx_item_stage;
ALTER TABLE item DROP COLUMN stage_id;
ALTER TABLE item RENAME COLUMN stage_tag_id TO stage_id;

ALTER TABLE item ADD CONSTRAINT fk_item_stage
    FOREIGN KEY (stage_id) REFERENCES category_tag(id) ON DELETE RESTRICT;
CREATE INDEX idx_item_stage ON item(stage_id);

-- ============================================================
-- url_alias
-- ============================================================
ALTER TABLE url_alias ADD COLUMN stage_tag_id UUID;

UPDATE url_alias SET stage_tag_id = (
    SELECT sc.tag_id FROM stage_config sc WHERE sc.machine_name = url_alias.stage_id
);
UPDATE url_alias SET stage_tag_id = '0193a5a0-0000-7000-8000-000000000001'::uuid
WHERE stage_tag_id IS NULL;

ALTER TABLE url_alias ALTER COLUMN stage_tag_id SET NOT NULL;
ALTER TABLE url_alias ALTER COLUMN stage_tag_id SET DEFAULT '0193a5a0-0000-7000-8000-000000000001'::uuid;

ALTER TABLE url_alias DROP COLUMN stage_id;
ALTER TABLE url_alias RENAME COLUMN stage_tag_id TO stage_id;

ALTER TABLE url_alias ADD CONSTRAINT fk_url_alias_stage
    FOREIGN KEY (stage_id) REFERENCES category_tag(id) ON DELETE RESTRICT;
CREATE INDEX idx_url_alias_stage ON url_alias(stage_id);

-- ============================================================
-- menu_link
-- ============================================================
ALTER TABLE menu_link ADD COLUMN stage_tag_id UUID;

UPDATE menu_link SET stage_tag_id = (
    SELECT sc.tag_id FROM stage_config sc WHERE sc.machine_name = menu_link.stage_id
);
UPDATE menu_link SET stage_tag_id = '0193a5a0-0000-7000-8000-000000000001'::uuid
WHERE stage_tag_id IS NULL;

ALTER TABLE menu_link ALTER COLUMN stage_tag_id SET NOT NULL;
ALTER TABLE menu_link ALTER COLUMN stage_tag_id SET DEFAULT '0193a5a0-0000-7000-8000-000000000001'::uuid;

ALTER TABLE menu_link DROP COLUMN stage_id;
ALTER TABLE menu_link RENAME COLUMN stage_tag_id TO stage_id;

ALTER TABLE menu_link ADD CONSTRAINT fk_menu_link_stage
    FOREIGN KEY (stage_id) REFERENCES category_tag(id) ON DELETE RESTRICT;
CREATE INDEX idx_menu_link_stage ON menu_link(stage_id);

-- ============================================================
-- tile
-- ============================================================
ALTER TABLE tile ADD COLUMN stage_tag_id UUID;

UPDATE tile SET stage_tag_id = (
    SELECT sc.tag_id FROM stage_config sc WHERE sc.machine_name = tile.stage_id
);
UPDATE tile SET stage_tag_id = '0193a5a0-0000-7000-8000-000000000001'::uuid
WHERE stage_tag_id IS NULL;

ALTER TABLE tile ALTER COLUMN stage_tag_id SET NOT NULL;
ALTER TABLE tile ALTER COLUMN stage_tag_id SET DEFAULT '0193a5a0-0000-7000-8000-000000000001'::uuid;

ALTER TABLE tile DROP COLUMN stage_id;
ALTER TABLE tile RENAME COLUMN stage_tag_id TO stage_id;

ALTER TABLE tile ADD CONSTRAINT fk_tile_stage
    FOREIGN KEY (stage_id) REFERENCES category_tag(id) ON DELETE RESTRICT;
CREATE INDEX idx_tile_stage ON tile(stage_id);

-- ============================================================
-- config_stage_association
-- ============================================================
-- This table uses (stage_id, entity_type, entity_id) as a composite PK.
-- We need to drop the PK, migrate, and recreate.

ALTER TABLE config_stage_association DROP CONSTRAINT config_stage_association_pkey;
DROP INDEX IF EXISTS idx_config_stage_assoc_stage;

ALTER TABLE config_stage_association ADD COLUMN stage_tag_id UUID;

UPDATE config_stage_association SET stage_tag_id = (
    SELECT sc.tag_id FROM stage_config sc WHERE sc.machine_name = config_stage_association.stage_id
);
UPDATE config_stage_association SET stage_tag_id = '0193a5a0-0000-7000-8000-000000000001'::uuid
WHERE stage_tag_id IS NULL;

ALTER TABLE config_stage_association ALTER COLUMN stage_tag_id SET NOT NULL;

ALTER TABLE config_stage_association DROP COLUMN stage_id;
ALTER TABLE config_stage_association RENAME COLUMN stage_tag_id TO stage_id;

ALTER TABLE config_stage_association ADD PRIMARY KEY (stage_id, entity_type, entity_id);
ALTER TABLE config_stage_association ADD CONSTRAINT fk_config_stage_assoc_stage
    FOREIGN KEY (stage_id) REFERENCES category_tag(id) ON DELETE RESTRICT;
CREATE INDEX idx_config_stage_assoc_stage ON config_stage_association(stage_id);

-- ============================================================
-- stage_deletion
-- ============================================================
ALTER TABLE stage_deletion DROP CONSTRAINT stage_deletion_pkey;

ALTER TABLE stage_deletion ADD COLUMN stage_tag_id UUID;

UPDATE stage_deletion SET stage_tag_id = (
    SELECT sc.tag_id FROM stage_config sc WHERE sc.machine_name = stage_deletion.stage_id
);
UPDATE stage_deletion SET stage_tag_id = '0193a5a0-0000-7000-8000-000000000001'::uuid
WHERE stage_tag_id IS NULL;

ALTER TABLE stage_deletion ALTER COLUMN stage_tag_id SET NOT NULL;

ALTER TABLE stage_deletion DROP COLUMN stage_id;
ALTER TABLE stage_deletion RENAME COLUMN stage_tag_id TO stage_id;

ALTER TABLE stage_deletion ADD PRIMARY KEY (stage_id, entity_type, entity_id);
ALTER TABLE stage_deletion ADD CONSTRAINT fk_stage_deletion_stage
    FOREIGN KEY (stage_id) REFERENCES category_tag(id) ON DELETE RESTRICT;
