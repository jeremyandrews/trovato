-- Create image_style table.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS image_style (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL UNIQUE,
    label VARCHAR(255) NOT NULL,
    effects JSONB NOT NULL DEFAULT '[]',
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
    changed BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint
);
