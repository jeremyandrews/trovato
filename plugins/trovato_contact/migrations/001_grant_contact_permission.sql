-- The contact form's permission, `access site-wide contact form`, granted to
-- the anonymous and authenticated roles: before the permission existed both
-- routes were open to anybody, and a site that does nothing keeps that.
-- Runs once per site (plugin_migration), so a role a site later revokes it
-- from stays revoked. Forward-only migration; no rollback.

INSERT INTO role_permissions (role_id, permission)
VALUES
    ('00000000-0000-0000-0000-000000000001', 'access site-wide contact form'),
    ('00000000-0000-0000-0000-000000000002', 'access site-wide contact form')
ON CONFLICT (role_id, permission) DO NOTHING;
