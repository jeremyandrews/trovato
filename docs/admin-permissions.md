# Admin surface permissions

Every administrative route in the kernel names the permission it requires. This
table is the whole list; it is what `crates/kernel/src/routes/` actually checks,
not a summary of intent.

Before this table existed, all but a handful of these routes called
`require_admin`, which gates on the `users.is_admin` column. That column is not
a permission: it cannot be granted to a role, it cannot be named in a
`role.*.yml` config file, and it does not appear in the permission grid. The
practical consequence was that a role holding a plugin's own permissions still
could not use the admin UI to exercise them: a role granted `administer argus`
could not open `/admin/content/add/argus_feed`, because that screen asked
whether you were a superuser rather than whether you could create that content.

## How the checks behave

`require_permission` (`crates/kernel/src/routes/helpers.rs`) is the single
helper every row below goes through. It:

- redirects to `/user/login` when there is no valid session, or the user is
  blocked;
- returns the user unconditionally when `users.is_admin` is set, so **the
  superuser bypass is preserved on every route in this table**;
- otherwise returns the user if any of their roles grants the named permission,
  and 403 if not.

So for a user without the permission, every route below is exactly as strict as
it was when it called `require_admin`, and for a superuser nothing changed at
all. The only behaviour that is new is the middle case: a non-superuser role
that holds the permission now gets in.

## Content

The admin content screens check the same permissions the public `/item/*`
routes check, so the two ways into the same operation agree.

| Route | Method | Permission |
|---|---|---|
| `/admin/content` | GET | `edit any content` |
| `/admin/content/add` | GET | `create content` |
| `/admin/content/add/{type}` | GET, POST | `create {type} content` |
| `/admin/content/{id}/edit` | GET, POST | `edit any content` |
| `/admin/content/{id}/delete` | POST | `delete any content` |
| `/admin/content/bulk` | POST | `edit any content`, or `delete any content` when the action is `delete` |

`/admin/content/add/{type}` builds its permission from the type in the path,
which is the convention `/item/add/{type}` already used, and is what makes a
plugin's content types reachable from the admin UI by a role rather than only by
a superuser. `/admin/content/bulk` is gated on the action it was asked to
perform, so a role that may publish cannot obtain a delete by routing it through
the bulk endpoint.

## Comments, files and media

| Route | Method | Permission |
|---|---|---|
| `/admin/content/comments` and its edit, status, settings and delete routes | GET, POST | `administer comments` |
| `/admin/content/files` | GET | `access files` |
| `/admin/content/files/{id}` | GET | `access files` |
| `/admin/content/files/{id}/delete` | POST | `administer files` |
| `/admin/content/files/{id}/alt-text` | POST | `administer files` |
| `/admin/media` | GET | `access files` |

## People

| Route | Method | Permission |
|---|---|---|
| `/admin/people` and all add, edit and delete routes | GET, POST | `administer users` |
| `/admin/people/roles` and all add, edit and delete routes | GET, POST | `administer users` |
| `/admin/people/permissions` | GET, POST | `administer users` |
| `/admin/users/{id}/sessions` and its revoke route | GET, POST | `administer users` |
| `/admin/recovery` | GET, POST | `administer users` |

**Role membership is delegable; the permissions it can carry are not.**
The user add and edit forms carry a checkbox per role. `administer users` is a
grantable permission and roles carry permissions, so without a guard a delegated
user administrator could assign themselves a role holding permissions well
beyond their own — `administer site` and the structure screens, say — by way of
the screen they were given to manage usernames. (Since BL-33 that is an
escalation of permissions rather than a promotion to site administrator:
`administer site` is an ordinary permission and the superuser column is not
grantable here at all.) A non-superuser may therefore only grant or revoke a role whose
permissions they already hold themselves: they can hand out what they have and
no more. A superuser is unrestricted. Roles the actor may not touch are left
exactly as they were on the target rather than silently dropped.

The CLI has the same two operations without the guard, because it is not
reachable over the network and whoever runs it already has the database:
`trovato user role-add <username> <role>`, `trovato user role-remove`, and
`trovato user roles <username>` to see what someone holds. A CLI change is not
seen by a running server until its permission cache expires, which the commands
say; that is the same limitation `config import` has always had, for the same
reason.

**The superuser flag itself is not delegated.** The user add and edit forms
carry an `is_admin` checkbox. `administer users` is a grantable permission and
the superuser flag is what grants it, so honouring that checkbox for a
non-superuser would turn `administer users` into a self-escalation to superuser,
and would equally let a delegated user administrator revoke the real superusers
and lock them out. The check is therefore on the field rather than the route:
only a superuser may set or clear `is_admin`, and for anyone else the stored
value is preserved whatever the form submitted. Everything else about a user
stays delegable.

## Taxonomy

| Route | Method | Permission |
|---|---|---|
| `/admin/structure/categories` and all its add, edit, delete and tag routes | GET, POST | `administer categories` |

`trovato_categories` also declares `create category terms`, `edit category
terms` and `delete category terms`. The tag routes could be split across those
three, and deliberately are not: they are declared by a plugin, so on a site
where that plugin is disabled they could not be granted to anyone, and the
screens would become superuser-only again through the back door.
`administer categories` is in `KERNEL_PERMISSIONS`, so it is always grantable.

## Site structure and configuration

These screens have no more specific permission declared anywhere in the kernel
or in a shipped plugin, so they take `administer site`, which is the declared
catch-all. No parallel name was invented for them.

This is still a real change: `administer site` is a grantable permission that a
role can hold and a `role.*.yml` file can name, where `users.is_admin` is
neither.

| Route | Method | Permission |
|---|---|---|
| `/system/ajax` | POST | `administer site` |
| `/admin/structure/types` and all its field and search routes | GET, POST | `administer site` |
| `/admin/structure/records` and its listing routes | GET | `administer site` |
| `/admin/structure/menus` and all its link routes | GET, POST | `administer site` |
| `/admin/structure/stages` and all its routes | GET, POST | `administer site` |
| `/admin/structure/tiles` and all its routes | GET, POST | `administer site` |
| `/admin/structure/aliases` and all its routes | GET, POST | `administer site` |
| `/admin/gather` and all its routes | GET, POST | `administer site` |
| `/admin/config/site` and its test-email route | GET, POST | `administer site` |
| `/admin/config/pathauto` and its regenerate route | GET, POST | `administer site` |
| `/cron/status` | GET | `administer site` |

## AI

| Route | Method | Permission |
|---|---|---|
| `/admin/config/ai/features` | GET, POST | `configure ai` |
| The AI provider, budget, chat and assistant screens | GET, POST | `configure ai`, and `view ai usage` for usage reporting |

## Translation

| Route | Method | Permission |
|---|---|---|
| `/admin/content/{id}/translate` | GET | `translate content` |
| `/admin/content/{id}/translate/{lang}` | GET, POST | `translate content` |
| `/admin/content/{id}/translate/{lang}/delete` | POST | `translate content` |

## Superuser only

Two routes keep `require_admin` and are not delegable by permission, because
they change who holds privilege rather than exercising it. Enabling a plugin
runs that plugin's code in the kernel and can introduce new permissions, new
routes and new tables, so it is not something a permission should be able to
grant.

| Route | Method | Check |
|---|---|---|
| `/admin/plugins` | GET | `require_admin` |
| `/admin/plugins/toggle` | POST | `require_admin` |

Site installation is also superuser-territory, but it runs before any user
exists and is gated by the installer rather than by a route check, so it does
not appear here.

## Admission to the section

`/admin` takes `access administration pages`, which is admission to the
administration section and nothing more. It is the weakest permission in
`KERNEL_PERMISSIONS` by design: every screen listed above still asks for its
own, so this one opens the door and confers no authority inside it.

It exists because the conversion above left a gap at the front door. Every
screen became delegable and the dashboard did not, so a role granted only
`administer comments` could reach `/admin/content/comments` by typing the
address and got 403 on the page that would have linked to it.

| Route | Method | Permission |
|---|---|---|
| `/admin` | GET | `access administration pages` |

Nothing implies it and it implies nothing. The single exception is a one-time
migration that grants it to every role already holding `administer site`, so an
upgrading site does not lose its dashboard; `administer site` does not imply it
afterwards, and a role granted `administer site` from now on gets exactly that.
A delegated role needs this permission added before it can use the dashboard,
which is new capability rather than a regression — see
[UPGRADING.md](../UPGRADING.md).

The dashboard's own cards are filtered to what the viewer may open: the
structure card needs `administer site`, and each "Add *type*" link needs the
same `create {type} content` string `/item/add/{type}` checks. The page
therefore never offers a door that answers 403.

The update banner, which names the running version and whether a security
release is outstanding, stays on `administer site` rather than following the
page. Someone admitted to moderate comments cannot act on an update, so opening
the section wider does not widen that disclosure.

**The admin layout's sidebar is not filtered.** It lists every screen
unconditionally, as it has since the conversion above made those screens
delegable, so a delegated role sees links it cannot open. That is not
introduced by this permission and is not fixed by it: filtering the sidebar
means giving `render_admin_template` the viewer, a signature change across its
80 call sites, and is left for that change.

| Route | Method | Check |
|---|---|---|
| `/system/ajax` | POST | `administer site` |

`/system/ajax` keeps `administer site` because it does work rather than admit:
it serves the admin forms' AJAX callbacks.
