-- Restore unique constraints lost during stage_id VARCHAR→UUID migration.
--
-- The migration 20260225000003_migrate_stage_fks.sql dropped and recreated
-- the stage_id column, which silently dropped these composite UNIQUE
-- constraints originally defined in the CREATE TABLE migrations.
--
-- Uses CREATE UNIQUE INDEX IF NOT EXISTS to be idempotent — safe on both
-- databases that ran the patched 20260225000003 and those that did not.

-- url_alias: UNIQUE (alias, language, stage_id)
CREATE UNIQUE INDEX IF NOT EXISTS uq_url_alias_alias_lang_stage
    ON url_alias (alias, language, stage_id);

-- menu_link: UNIQUE (path, menu_name, stage_id)
CREATE UNIQUE INDEX IF NOT EXISTS uq_menu_link_path_menu_stage
    ON menu_link (path, menu_name, stage_id);
