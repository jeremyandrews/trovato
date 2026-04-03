-- Create redirect table for URL redirect management.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS redirect (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source VARCHAR(2048) NOT NULL,
    destination VARCHAR(2048) NOT NULL,
    status_code SMALLINT NOT NULL DEFAULT 301,
    language VARCHAR(12) NOT NULL DEFAULT 'en',
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint
);

CREATE INDEX IF NOT EXISTS idx_redirect_source ON redirect (source);
CREATE INDEX IF NOT EXISTS idx_redirect_language ON redirect (language);
