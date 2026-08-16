-- URL Alias table for human-readable URLs
-- Maps alias paths (e.g., /about-us) to source paths (e.g., /item/{uuid})

CREATE TABLE IF NOT EXISTS url_alias (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source VARCHAR(255) NOT NULL,
    alias VARCHAR(255) NOT NULL,
    language VARCHAR(12) NOT NULL DEFAULT 'en',
    stage_id VARCHAR(50) NOT NULL DEFAULT 'live',
    created BIGINT NOT NULL,
    UNIQUE (alias, language, stage_id)
);

-- Index for fast alias lookups (most common query)
CREATE INDEX IF NOT EXISTS idx_url_alias_alias ON url_alias (alias);

-- Index for finding all aliases for a given source
CREATE INDEX IF NOT EXISTS idx_url_alias_source ON url_alias (source);
