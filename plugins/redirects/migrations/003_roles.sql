-- Redirect admin role.
-- Forward-only migration; no rollback.

INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'redirect_admin')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('administer redirects'),
    ('view redirects')
) AS p(perm)
WHERE r.name = 'redirect_admin'
ON CONFLICT (role_id, permission) DO NOTHING;
