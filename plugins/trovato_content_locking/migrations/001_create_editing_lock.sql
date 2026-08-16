-- Create editing_lock table for pessimistic content locking.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS editing_lock (
    entity_type VARCHAR(255) NOT NULL,
    entity_id VARCHAR(255) NOT NULL,
    user_id UUID NOT NULL,
    locked_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
    expires_at BIGINT NOT NULL,
    PRIMARY KEY (entity_type, entity_id)
);
