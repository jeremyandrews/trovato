-- Create item_translation table for field-level content translation.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS item_translation (
    item_id UUID NOT NULL,
    language VARCHAR(12) NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    fields JSONB NOT NULL DEFAULT '{}',
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
    changed BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
    PRIMARY KEY (item_id, language)
);
