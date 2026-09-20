-- Grant `access administration pages` to every role that already holds
-- `administer site`, so no existing site loses its dashboard on upgrade.
-- Forward-only migration; no rollback.
--
-- `/admin` used to take `administer site`. It now takes the new, weaker
-- `access administration pages`, which is admission to the administration
-- section and confers no authority inside it. Without this grant, a role that
-- administered the site yesterday would open the dashboard tomorrow and be
-- refused by a permission that did not exist when the site was configured.
--
-- **This is the only automatic grant.** It is a one-time correction for a
-- permission that was split in two, not a rule: nothing in the kernel makes
-- `administer site` imply `access administration pages` afterwards, and a role
-- granted `administer site` from now on gets exactly that. A site that wants a
-- delegated role in the section — one holding `administer comments`, say —
-- grants it this permission deliberately, at the grid or in a `role.*.yml`.
--
-- Superusers (the `users.is_admin` column) need nothing here: they hold every
-- permission by the one bypass the kernel has.
--
-- `ON CONFLICT DO NOTHING` because a site may already have granted it by hand
-- between the permission landing and this migration running, and because a
-- migration that cannot be re-run on a restored database is a migration that
-- fails at the worst moment.
INSERT INTO role_permissions (role_id, permission)
SELECT role_id, 'access administration pages'
FROM role_permissions
WHERE permission = 'administer site'
ON CONFLICT DO NOTHING;
