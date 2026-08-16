-- Multi-tenancy schema (Story 46.1).
-- Creates the tenant table and seeds the default tenant.
-- All subsequent migrations add tenant_id to content tables.

CREATE TABLE IF NOT EXISTS tenant (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    machine_name VARCHAR(128) UNIQUE NOT NULL,
    status BOOLEAN DEFAULT TRUE,
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    data JSONB DEFAULT '{}'::jsonb
);

-- Default tenant: deterministic UUID matching DEFAULT_TENANT_ID constant in kernel.
-- Single-tenant sites use this tenant for all content.
INSERT INTO tenant (id, name, machine_name, status, created)
VALUES (
    '0193a5a0-0001-7000-8000-000000000001',
    'Default',
    'default',
    TRUE,
    EXTRACT(EPOCH FROM NOW())::BIGINT
) ON CONFLICT DO NOTHING;
