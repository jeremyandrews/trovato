-- Goose auth roles: goose_operator (full CRUD) and goose_viewer (read-only).
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

-- goose_operator: full CRUD on all goose_* types
INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'goose_operator')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access content'),
    ('view goose_test_run content'),
    ('create goose_test_run content'),
    ('edit goose_test_run content'),
    ('delete goose_test_run content'),
    ('view goose_scenario content'),
    ('create goose_scenario content'),
    ('edit goose_scenario content'),
    ('delete goose_scenario content'),
    ('view goose_endpoint_result content'),
    ('create goose_endpoint_result content'),
    ('edit goose_endpoint_result content'),
    ('delete goose_endpoint_result content'),
    ('view goose_site content'),
    ('create goose_site content'),
    ('edit goose_site content'),
    ('delete goose_site content'),
    ('view goose_comparison content'),
    ('create goose_comparison content'),
    ('edit goose_comparison content'),
    ('delete goose_comparison content')
) AS p(perm)
WHERE r.name = 'goose_operator'
ON CONFLICT (role_id, permission) DO NOTHING;

-- goose_viewer: read-only access
INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'goose_viewer')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access content')
) AS p(perm)
WHERE r.name = 'goose_viewer'
ON CONFLICT (role_id, permission) DO NOTHING;
