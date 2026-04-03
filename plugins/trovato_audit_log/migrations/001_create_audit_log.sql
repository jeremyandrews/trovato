-- Create audit_log table for tracking actions.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS audit_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    action VARCHAR(255) NOT NULL,
    entity_type VARCHAR(255) NOT NULL,
    entity_id VARCHAR(255) NOT NULL DEFAULT '',
    user_id UUID,
    ip_address VARCHAR(45) NOT NULL DEFAULT '',
    details JSONB NOT NULL DEFAULT '{}',
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint
);

CREATE INDEX IF NOT EXISTS idx_audit_log_created ON audit_log (created);
CREATE INDEX IF NOT EXISTS idx_audit_log_user_id ON audit_log (user_id);
CREATE INDEX IF NOT EXISTS idx_audit_log_entity ON audit_log (entity_type, entity_id);
