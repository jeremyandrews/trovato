-- Restore unique constraints lost during stage_id VARCHAR→UUID migration.
--
-- The migration 20260225000003_migrate_stage_fks.sql dropped and recreated
-- the stage_id column, which silently dropped these composite UNIQUE
-- constraints originally defined in the CREATE TABLE migrations.

-- url_alias: UNIQUE (alias, language, stage_id)
ALTER TABLE url_alias
    ADD CONSTRAINT uq_url_alias_alias_lang_stage UNIQUE (alias, language, stage_id);

-- menu_link: UNIQUE (path, menu_name, stage_id)
ALTER TABLE menu_link
    ADD CONSTRAINT uq_menu_link_path_menu_stage UNIQUE (path, menu_name, stage_id);
