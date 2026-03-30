-- User-tenant junction table (Story 46.2).
-- Users are global; this table links them to tenants with per-tenant roles.
-- Admin users (is_admin=true) can access all tenants without junction entries.

CREATE TABLE IF NOT EXISTS user_tenant (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    is_active BOOLEAN DEFAULT TRUE,
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    PRIMARY KEY (user_id, tenant_id)
);

-- Seed all existing users into the default tenant
INSERT INTO user_tenant (user_id, tenant_id, is_active, created)
SELECT id, '0193a5a0-0001-7000-8000-000000000001', TRUE, EXTRACT(EPOCH FROM NOW())::BIGINT
FROM users
ON CONFLICT DO NOTHING;
