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

Its JSON sibling, `require_permission_json`, asks the same question and answers
in `AppError` JSON instead: **401** when nobody is signed in (or the session's
user is blocked or gone), **403** when a signed-in user lacks the permission.
`require_login` and `require_login_json` are the same pair without a permission,
and `refusal_for` gives the same 401-or-403 answer for a handler that already
holds the viewer. That is the one refusal rule: no route decides it for itself.

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
| `/admin/content/bulk` | POST | `edit any content`; plus `publish content` when the action is `publish` or `unpublish`; `delete any content` when it is `delete` |

`/admin/content/add/{type}` builds its permission from the type in the path,
which is the convention `/item/add/{type}` already used, and is what makes a
plugin's content types reachable from the admin UI by a role rather than only by
a superuser. `/admin/content/bulk` is gated on the action it was asked to
perform, so a role that may publish cannot obtain a delete by routing it through
the bulk endpoint.

### Publishing is its own permission

`publish content` decides whether a user may set an item's published state, and
it is checked on every item write — the admin forms, `/item/add/{type}`,
`/item/{id}/edit`, a revert, and the MCP `create_item` and `update_item` tools.
Without it:

- a create that asks to publish is refused, and a create that says nothing
  stores a draft (the model layer's default is published, so silence has to be
  answered rather than passed on);
- an update or a revert that changes the published state is refused in either
  direction, and one that leaves it alone saves normally;
- the forms render no published checkbox at all, so the ordinary case never
  produces a refusal.

It is site-wide, like `create content` and `edit any content`: a per-type split
would need a permission for every type a plugin declares. A migration
(`20261004000001_grant_publish_content.sql`) grants it once to every role that
already held `create content`, `edit own content`, `edit any content` or a
per-type `create … content`, so no existing site loses the ability to publish on
upgrade. Like the admission grant above, that is a one-time correction and not a
rule: nothing makes those permissions imply this one afterwards.

### Publishing options

The item forms have a "Publishing options" section (B107). Promoted to front
page and Sticky at top of lists take `publish content` or `administer content`;
Authored by and Authored on take `administer content`. The section shows only
what the user may set, and `ItemService` checks every write again on every
path. Without the permission, a change to any of the four is refused. Asking
for the value already stored is no change. On a create, an author other than
the creator or an authored on time counts as a change. `administer content` is
granted by no migration, and an administrator holds it implicitly.

Field-level edit access is enforced on the same writes. A field a
`tap_field_access` plugin denies this user for the `edit` operation cannot be
changed by them, the forms do not render it as an input, and a submission that
leaves it out keeps the stored value rather than erasing it.

### Revisions

These routes are under `/item/*` rather than `/admin/*`, like the rest of the
revision history (`/item/{id}/revisions` and its preview and compare), but
belong in this table for the same reason the publishing permissions above do:
they are permission-gated rather than `require_admin`-gated.

| Route | Method | Permission |
|---|---|---|
| `/item/{id}/revisions/{rev}/delete` | GET, POST | `delete revisions`, and delete access to the item |

The GET is a confirm page, not an immediate delete: it checks the same two
things the POST does and answers the same way, so a user who cannot delete
never sees a form offering to. Both refuse the item's current revision with
**409**, a user who holds the permission but not delete access on this item
with **403**, and a revision that does not belong to the item (or does not
exist) with **404**. `ItemService::delete_revision` is the one path, whether
reached from this route or from `trovato item revision-delete <item>
<revision>` (`docs/cli-reference.md`), and it is never the item's current
revision: the item row points at it, and the foreign key from
`item.current_revision_id` to `item_revision.id` refuses that delete at the
database even if the service check above it did not.

`view revisions` and `delete revisions` are granted by no migration; an
administrator holds both implicitly, as every permission here.

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
| `/admin/people` and all add, edit and delete routes | GET, POST | `administer users`; a non-superuser may edit, reset, block or delete only an account whose permissions they all hold, and never a superuser's |
| `/admin/people/roles` and all add, edit and delete routes | GET, POST | `administer users`; a non-superuser may delete only a role whose permissions they all hold |
| `/admin/people/permissions` | GET, POST | `administer users`; a non-superuser may grant or revoke only a permission they hold |
| `/admin/users/{id}/sessions` and its revoke route | GET, POST | `administer users` |
| `/admin/recovery` | GET, POST | `administer users` |
| `/admin/people/rules`, its check box, and its add, edit and delete routes | GET, POST | `administer access rules` |

`administer access rules` is separate from `administer users` on purpose: the
rules decide who may register and sign in at all, which is a policy over every
account rather than the management of any one. No migration grants it; an
administrator holds it implicitly. A user administrator creating an account a
rule blocks is warned on `/admin/people`, not refused.

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
value is preserved whatever the form submitted.

**One rule covers every screen here: a non-superuser acts only within the
permissions they hold.** The role checkboxes above were the first place it was
applied, and it applies to the rest of `/admin/people` the same way. A holder of
`administer users` who is not a superuser:

- may edit, reset the password of, block or delete an account only when every
  permission that account holds is one they hold too. A superuser's account is
  never within reach, since a superuser's authority is not a set of permissions.
  The edit form is refused with a 403 rather than shown, and so is the submit,
  before anything about the account changes, roles and password included.
- may delete a role only when every permission it carries is one they hold,
  since deleting it takes those permissions from every member.
- may grant or revoke on the permission grid only the permissions they hold.
  Any other box is left exactly as it was on every role, and the save still
  redirects back to the grid.

A superuser is unrestricted, with one exception: **the last active superuser
cannot be demoted, blocked or deleted** from these screens. A site with no active
superuser cannot be administered back into having one. Self service account
deletion has had the same guard; the edit and delete routes here now share it.

The grid also ignores, for everyone, any permission name it did not render. A
name outside `KERNEL_PERMISSIONS` and the enabled plugins' declared permissions
can only have been typed into the request, and nothing checks it.

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

### Owned vocabularies

A plugin may declare a kind of owner (a group, a club) whose vocabularies the
kernel serves under a route the plugin names, e.g. `/group/{owner}/categories`.
See "Owned vocabularies" in `docs/plugin-development.md`.

| Route | Method | Who |
|---|---|---|
| `<owner route>`, `/{vocabulary}`, `/{vocabulary}/add`, `/{vocabulary}/terms/{term}/edit` and `/delete` | GET, POST | a signed-in user the plugin's `tap_vocabulary_owner_access` grants for that owner, or a holder of the permission the plugin declares for the owner type, or a superuser |

An anonymous user is sent to sign in, and a user who is refused gets 403. A
vocabulary or term of another owner is a 404 under this owner's route. Owned
vocabularies are also listed at `/admin/structure/categories`, with their
owner, for holders of `administer categories`.

## Site structure and configuration

These screens have no more specific permission declared anywhere in the kernel
or in a shipped plugin, so they take `administer site`, which is the declared
catch-all. No parallel name was invented for them.

This is still a real change: `administer site` is a grantable permission that a
role can hold and a `role.*.yml` file can name, where `users.is_admin` is
neither.

| Route | Method | Permission |
|---|---|---|
| `/system/ajax` | POST | `administer site` (until B71 Part 4; see below) |
| `/admin/structure/types` and all its field and search routes | GET, POST | `administer site` |
| `/admin/structure/records` and its listing routes | GET | `administer site` |
| `/admin/structure/menus` and all its link routes | GET, POST | `administer site` |
| `/admin/structure/stages` and all its routes | GET, POST | `administer site` |
| `/admin/structure/tiles` and all its routes | GET, POST | `administer site` |
| `/admin/structure/aliases` and all its routes | GET, POST | `administer site` |
| `/admin/gather` and all its routes | GET, POST | `administer site` |
| `POST /api/v1/gather/query`, the ad hoc gather the builder preview calls | POST | `administer site` |
| `/admin/config/site` and its test-email route | GET, POST | `administer site` |
| `/admin/config/pathauto` and its regenerate route | GET, POST | `administer site` |
| `/cron/status` | GET | `administer site` |
| `/admin/config/site/maintenance`, the maintenance mode toggle | POST | `administer site` |
| `/admin/reports/cron`, the cron report and its Run now button | GET, POST | `administer site` |
| `/admin/reports/log`, the site log | GET | `administer site` |

While maintenance mode is on, a signed-in user without `bypass maintenance mode`
gets the maintenance page like an anonymous visitor. Administrators hold it
implicitly; grant it to a role whose members must keep working during the window.

## Redirects

| Route | Method | Permission |
|---|---|---|
| `/admin/config/redirects` | GET | `view redirects` |
| `/admin/config/redirects/add`, `/{id}/edit` and `/{id}/delete` | GET, POST | `administer redirects` |

Both are declared by `trovato_redirects`, so neither is grantable on a site
where that plugin is disabled (the routes themselves 404 there too). The JSON
API mirrors this: `GET /api/v1/redirects` and `GET /api/v1/redirect/{id}` take
`view redirects`; `POST /api/v1/redirect`, `PUT /api/v1/redirect/{id}` and
`DELETE /api/v1/redirect/{id}` take `administer redirects` and the
`X-CSRF-Token` header.

## Appearance

| Route | Method | Permission |
|---|---|---|
| `/admin/appearance` and a theme's screenshot | GET, POST | `administer themes` |

The default theme draws every page outside `/admin`, the administration theme
every page under it. No migration grants `administer themes`; an administrator
holds it implicitly.

## Text formats

| Route | Method | Permission |
|---|---|---|
| `/admin/config/formats` and its add, edit and delete routes | GET, POST | `administer text formats` |

Who may use a format is the `use <format>` permission, as it always was for
`filtered_html` and `full_html`; a format the site adds gets its own, and the
permission grid lists it. The format's edit form ticks roles for that
permission. A non-superuser may change it only for a format whose permission
they hold themselves, the grid's rule; otherwise the role boxes are shown
disabled and the save leaves the grants as they were. `plain_text` needs no
permission. The three built-in formats cannot be deleted and their filter
cannot be changed.

## AI

| Route | Method | Permission |
|---|---|---|
| The AI provider, budget, chat and assistant screens | GET, POST | `configure ai`, and `view ai usage` for usage reporting |

## Translation

| Route | Method | Permission |
|---|---|---|
| `/admin/content/{id}/translate` | GET | `translate content` |
| `/admin/content/{id}/translate/{lang}` | GET, POST | `translate content` |
| `/admin/content/{id}/translate/{lang}/delete` | POST | `translate content` |

## Search

`/search` and `/api/v1/search` need `search content` and `access content`.

Server search covers Live and, for a `use stages` holder, the stage they
switched to. It skips a stage whose Visibility is Internal, the default for a
new stage. On the stages it covers, a visitor finds:

- every published item (a plugin cannot refuse one to a visitor with
  `access content`);
- their own unpublished items, if they are signed in and hold
  `view own unpublished content`, `view own content` or `view any content`.

If a plugin denies an author their own unpublished item, the page hides it but
the total counts it.

A superuser finds every item on those stages, internal or not, published or
not.

No one else finds or counts another author's unpublished item, even with
`view any content`, nor an unpublished item, their own included, that only a
type-specific permission or a plugin opens.

Once the browser loads the Pagefind index, `/search` lists its results instead
for every visitor allowed to search: the published Live items `anonymous user`
could view when each was last indexed. Taking `access content` from that role
empties it only after `trovato search rebuild` and the next cron run.

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

The admin layout's sidebar is filtered the same way (BL-117, B71 Part 6). It
is built from one table, `routes::admin_nav::LINKS`, which names each screen
and the permission its page asks for, so a delegated role sees only the links
it may open.

`/system/ajax` no longer asks for `administer site` (B71 Part 4). It serves the
AJAX callbacks of every form built by the form pipeline, so it checks the
form's own access instead:
- a valid `X-CSRF-Token`;
- form state built for the caller;
- whether the caller may still open that form (edit access on the item, or the
  create permission for the type).

See `docs/plugin-development.md`.
