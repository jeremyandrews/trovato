-- Media admin role.
-- Forward-only migration; no rollback.

INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'media_admin')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access content'),
    ('view media content'),
    ('create media content'),
    ('edit media content'),
    ('delete media content')
) AS p(perm)
WHERE r.name = 'media_admin'
ON CONFLICT (role_id, permission) DO NOTHING;
