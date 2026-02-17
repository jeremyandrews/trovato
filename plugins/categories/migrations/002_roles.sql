-- Category admin role.
-- Forward-only migration; no rollback.

INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'category_admin')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('administer categories'),
    ('create category terms'),
    ('edit category terms'),
    ('delete category terms')
) AS p(perm)
WHERE r.name = 'category_admin'
ON CONFLICT (role_id, permission) DO NOTHING;
