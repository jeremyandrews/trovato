-- Argus auth roles: argus_admin (full CRUD) and argus_reader (read + react + discuss).
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

-- argus_admin: full CRUD on all argus_* types
INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'argus_admin')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access content'),
    ('view argus_article content'),
    ('create argus_article content'),
    ('edit argus_article content'),
    ('delete argus_article content'),
    ('view argus_story content'),
    ('create argus_story content'),
    ('edit argus_story content'),
    ('delete argus_story content'),
    ('view argus_topic content'),
    ('create argus_topic content'),
    ('edit argus_topic content'),
    ('delete argus_topic content'),
    ('view argus_feed content'),
    ('create argus_feed content'),
    ('edit argus_feed content'),
    ('delete argus_feed content'),
    ('view argus_entity content'),
    ('create argus_entity content'),
    ('edit argus_entity content'),
    ('delete argus_entity content'),
    ('view argus_reaction content'),
    ('create argus_reaction content'),
    ('edit argus_reaction content'),
    ('delete argus_reaction content'),
    ('view argus_discussion content'),
    ('create argus_discussion content'),
    ('edit argus_discussion content'),
    ('delete argus_discussion content')
) AS p(perm)
WHERE r.name = 'argus_admin'
ON CONFLICT (role_id, permission) DO NOTHING;

-- argus_reader: read access + create reactions + create discussions
INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'argus_reader')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access content'),
    ('create argus_reaction content'),
    ('create argus_discussion content')
) AS p(perm)
WHERE r.name = 'argus_reader'
ON CONFLICT (role_id, permission) DO NOTHING;
