-- Grant `publish content` to every role that can already create or edit
-- content, so no existing site loses the ability to publish on upgrade.
-- Forward-only migration; no rollback.
--
-- Whether an item is published used to come straight from the client — a
-- `status` key in a JSON body, a checkbox on the form — with no permission
-- behind it at all. `publish content` is that permission. Without this grant,
-- every role that published content yesterday would save an unpublished item
-- tomorrow, or be refused outright, because of a permission that did not exist
-- when the site was configured.
--
-- The selection is every role holding `create content`, `edit own content`,
-- `edit any content`, or a per-type create permission (`create % content`,
-- which is the shape `/item/add/{type}` and the admin add form build from the
-- type in the path, including the ones plugins declare). A role that can put
-- content into the site is a role that could publish it before today.
--
-- **This is a one-time correction, not a rule.** Nothing in the kernel makes
-- any of those permissions imply this one afterwards: a role granted
-- `create content` from now on gets exactly that, and a site that wants it to
-- publish grants this deliberately, at the permission grid or in a
-- `role.*.yml`. That is the whole point of splitting the authority out.
--
-- Superusers (the `users.is_admin` column) need nothing here: they hold every
-- permission by the one bypass the kernel has.
--
-- `ON CONFLICT DO NOTHING` because a site may already have granted it by hand
-- between the permission landing and this migration running, and because a
-- migration that cannot be re-run on a restored database is a migration that
-- fails at the worst moment.
INSERT INTO role_permissions (role_id, permission)
SELECT DISTINCT role_id, 'publish content'
FROM role_permissions
WHERE permission IN ('create content', 'edit own content', 'edit any content')
   OR permission LIKE 'create % content'
ON CONFLICT DO NOTHING;
