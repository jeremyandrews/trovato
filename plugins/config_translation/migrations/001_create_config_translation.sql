-- Create config_translation table for config entity translation overlay.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS config_translation (
    entity_type VARCHAR(255) NOT NULL,
    entity_id VARCHAR(255) NOT NULL,
    language VARCHAR(12) NOT NULL,
    data JSONB NOT NULL DEFAULT '{}',
    PRIMARY KEY (entity_type, entity_id, language)
);
