-- Create locale_string table for interface translations.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS locale_string (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source TEXT NOT NULL,
    translation TEXT NOT NULL,
    language VARCHAR(12) NOT NULL,
    context VARCHAR(255) NOT NULL DEFAULT '',
    UNIQUE (source, language, context)
);

CREATE INDEX IF NOT EXISTS idx_locale_string_language ON locale_string (language);
