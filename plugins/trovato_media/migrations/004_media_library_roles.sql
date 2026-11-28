-- B290 — the media library's flat permission set, granted to the existing
-- media_admin role alongside its original (now orphaned) per-type names from
-- 003_roles.sql. Forward-only migration; no rollback.

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access media overview'),
    ('create media'),
    ('edit own media'),
    ('edit any media'),
    ('delete own media'),
    ('delete any media')
) AS p(perm)
WHERE r.name = 'media_admin'
ON CONFLICT (role_id, permission) DO NOTHING;
