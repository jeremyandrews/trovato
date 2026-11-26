-- Grant `search content` to every role that can already read content, so no
-- existing site loses its search page on upgrade. Forward-only; no rollback.
--
-- `/search` and `/api/v1/search` had no permission of their own: every
-- visitor could search, and a site could not withhold search from anonymous
-- visitors or members the way Drupal's `search content` permission let it.
-- The routes now ask for `search content`. A role that could read content
-- yesterday could search it, so that is the selection here.
--
-- A one-time correction, not a rule: a role granted `access content` from now
-- on gets exactly that. Superusers (`users.is_admin`) hold every permission
-- already. `ON CONFLICT DO NOTHING` so the migration is safe to re-run.
INSERT INTO role_permissions (role_id, permission)
SELECT DISTINCT role_id, 'search content'
FROM role_permissions
WHERE permission = 'access content'
ON CONFLICT DO NOTHING;
