The content audit log records what changed. `AuditService::log` had no caller,
so the `audit_log` table `trovato_audit_log` creates was always empty, and the
plugin's `/admin/reports/audit` menu entry led to a 404. Now every write
service (items and their translations, comments, users, roles and their
grants, categories and tags, URL aliases, menu links, tiles and stages, and a
stage publish) records a row through one kernel helper,
`services::audit::AuditTrail`, when the plugin is enabled: the action, what it
was done to, who did it and from which address. Who acted comes from the
request the write is part of, scoped by a new `scope_audit_actor` middleware,
so a write a bulk action, a plugin or a JSON route makes is recorded the same
as one from an admin form. `/admin/reports/audit` lists the rows, newest first,
50 to a page, to holders of `view audit log`. Rows now carry time-ordered ids,
so two writes in the same second list in the order they happened, and a write
with no request behind it records no address rather than `invalid`.

Security events cover the paths that were missing. Password login emits
`auth.login_succeeded` and `auth.login_failed` (with the reason), as passkey
login always did. A password set by reset email, by account recovery, by an
administrator or from `trovato user reset-password` emits `password.changed`
(or `password.set` for an account that had none) from
`UserService::update_password`, with `details.via` saying which; the
self-service change, which emitted its own, now goes through the same place.
New kinds: `role.created`, `role.changed`, `role.deleted`,
`role.permissions_changed` (with what was granted and revoked),
`user.roles_changed` (a role assigned or removed, or the administrator flag)
and `user.status_changed` (blocked or unblocked), emitted by `RoleService` and
`UserService` with the acting administrator as the actor. `trovato user
role-add` and `role-remove` emit `user.roles_changed` too.
