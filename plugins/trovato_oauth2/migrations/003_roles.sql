-- OAuth admin role.
-- Forward-only migration; no rollback.

INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'oauth_admin')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('administer oauth clients')
) AS p(perm)
WHERE r.name = 'oauth_admin'
ON CONFLICT (role_id, permission) DO NOTHING;
