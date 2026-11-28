-- B278: `view timers` and `administer timers` (/admin/reports/timers), granted
-- to every role that already holds `administer site`, so the roles that run a
-- site today see and control its timers. Superusers (`users.is_admin`) hold
-- every permission already. Forward-only; `ON CONFLICT DO NOTHING` so it is
-- safe to re-run.
INSERT INTO role_permissions (role_id, permission)
SELECT DISTINCT role_id, p.permission
FROM role_permissions
CROSS JOIN (VALUES ('view timers'), ('administer timers')) AS p(permission)
WHERE role_permissions.permission = 'administer site'
ON CONFLICT DO NOTHING;
