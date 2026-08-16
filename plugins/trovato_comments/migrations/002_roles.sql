-- Comment moderator role.
-- Forward-only migration; no rollback.

INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'comment_moderator')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('administer comments'),
    ('post comments'),
    ('edit own comments'),
    ('skip comment approval')
) AS p(perm)
WHERE r.name = 'comment_moderator'
ON CONFLICT (role_id, permission) DO NOTHING;
