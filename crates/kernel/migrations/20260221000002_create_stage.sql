-- Explicit stage table with hierarchy support (upstream_id).
CREATE TABLE IF NOT EXISTS stage (
    id VARCHAR(64) PRIMARY KEY,
    label VARCHAR(255) NOT NULL,
    upstream_id VARCHAR(64) REFERENCES stage(id) ON DELETE SET NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'open',
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL
);

-- Seed the 'live' stage
INSERT INTO stage (id, label, upstream_id, status, created, changed)
VALUES ('live', 'Live', NULL, 'open', 0, 0)
ON CONFLICT (id) DO NOTHING;
