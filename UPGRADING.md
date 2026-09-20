# Upgrading

What an operator has to know or do before moving a running site to a newer
version. Only versions that need something are listed; a version that is not
here needs nothing beyond the ordinary upgrade.

Each entry says what changed, who it affects, and how to find out whether that
is you **before** you upgrade.

## Unreleased

### `administer site` no longer makes its holder a site administrator

**Who is affected:** a site that granted the `administer site` permission to a
role and relied on it as a blanket pass. If no role holds `administer site`,
nothing here applies to you.

**What changed.** The kernel had two notions of administrator that did not
agree. `require_admin` and `require_permission` read the `users.is_admin`
column, the superuser flag. The plugin-facing `UserContext::is_admin()` read
something else entirely: whether the permission list contained the string
`administer site`. So a role granted `administer site` was an administrator to
every check keyed on the context — the item routes among them — while still
being refused by `require_permission` on the admin screens themselves.

There is now one notion. The `users.is_admin` column travels on the request
context as itself, and `administer site` is an ordinary permission with no
structural meaning. It still opens the structure and configuration screens that
0.102 gated on it, and it no longer opens anything else.

**What you will notice.** A role holding `administer site` loses the implicit
pass it had on checks that ask for a *different* permission. The concrete case
is content: `/item/add/{type}` asks for `create {type} content`, and a role
whose only grant was `administer site` used to pass that check and will now be
refused. The same applies to the other context-based checks — item and field
visibility, menu entries, gather results, file serving, the AI assistant's
scope gate.

**What to do.** Grant those roles the permissions they actually need. This lists
every role holding `administer site` and how many users are in it, so you can
see whom it affects before you upgrade:

```sql
SELECT r.name AS role,
       count(ur.user_id) AS users
FROM roles r
JOIN role_permissions rp ON rp.role_id = r.id
LEFT JOIN user_roles ur ON ur.role_id = r.id
WHERE rp.permission = 'administer site'
GROUP BY r.name
ORDER BY users DESC, r.name;
```

For each role the query returns, decide what it was really using the blanket
pass for and grant that: the per-type `create {type} content`, `edit any
content`, `access files`, and so on. `/admin/people/permissions` is the screen;
`role.*.yml` is the file.

A user carrying the `users.is_admin` column is unaffected — they still hold
everything, and they now hold it on the plugin side too, which they did not
before. The superuser column remains non-delegable: it cannot be granted to a
role, and only a superuser may set or clear it.

**The other direction, which costs you nothing.** A plugin asking
`current-user-has-permission` now gets the effective answer, so a superuser is
no longer refused by a plugin's own check. Before, the context builder replaced
a superuser's real permissions with the `administer site` marker, so a plugin's
check saw the marker and nothing else: an administrator could open a plugin's
screen through the kernel's gate and have every action on it refuse them. A
plugin that wrote its own administrator special case can drop it; one that did
not now works for administrators without changes.
