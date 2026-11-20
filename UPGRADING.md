# Upgrading

What an operator has to know or do before moving a running site to a newer
version. Only versions that need something are listed; a version that is not
here needs nothing beyond the ordinary upgrade.

Each entry says what changed, who it affects, and how to find out whether that
is you **before** you upgrade.

## Unreleased

### Plugin mail from `tap_cron` and `tap_queue_worker` now sends

**Who is affected:** a plugin that calls `send-to-site-contacts`
(`host::mail_send_to_site_contacts`) from `tap_cron`, `tap_queue_worker` or
`tap_install`.

**What changed.** Those calls always failed with `ERR_MAIL_NOT_CONFIGURED`,
because background dispatch carried no email service, even on a site with SMTP
configured. They now send, through the same email service the kernel uses, and
each one is counted against the plugin's `mail` bucket (100 an hour by default,
`TROVATO_RATE_LIMIT_MAIL` or `rate_limit.mail` to change it), exactly as a call
from a request is.

**How to find out.** Search your plugin for `mail_send_to_site_contacts` in
code reached from those taps. A plugin that called it there and treated the
error as "mail is off" will now deliver; make sure that is what it should do,
and that a loop cannot spend the hour's bucket on mail nobody wants.

### SMTP settings stored in `/admin/config/site` are deleted; use the environment

**Who is affected:** a site whose administrator entered SMTP settings in the
"Email / SMTP" section of `/admin/config/site`.

**What changed.** Those values were never used for delivery. Mail has always
been sent with `SMTP_HOST`, `SMTP_PORT`, `SMTP_USERNAME`, `SMTP_PASSWORD`,
`SMTP_ENCRYPTION` and `SMTP_FROM_EMAIL` from the environment; the form only
stored its values, the password in clear text, in `site_config`. The form is
gone, the page now shows the effective environment configuration read only, and
migration `20261121000001_remove_site_config_smtp` deletes the six keys
(`smtp_host`, `smtp_port`, `smtp_username`, `smtp_password`,
`smtp_encryption`, `smtp_from`).

**How to find out, before upgrading.** Read back what was stored:

```sql
SELECT key, value FROM site_config WHERE key LIKE 'smtp_%';
```

No rows means nothing to do. Otherwise, if those are the settings you meant
mail to use, set them in the environment (`SMTP_HOST`, `SMTP_PORT`,
`SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_ENCRYPTION`, `SMTP_FROM_EMAIL`) and
restart; the migration deletes the rows on upgrade. A stored password was
readable to anyone with database or backup access, so rotate it.

### File fields are indexed and promoted; uploads to 10 MB; served type is the stored type

**Who is affected:** a site with File fields; anyone who saves items through
the item API, a plugin or `config import`; a reverse proxy in front of uploads.

**What changed.** Migration `20261120100001_index_file_field_references`
records a `file_reference` row for every file id an existing item's File field
holds (read from the item type's field definitions), then promotes every
`Temporary` file that has a reference to `Permanent`. It is additive and safe to
rerun. Reversing it means deleting the backfilled rows
(`DELETE FROM file_reference` for the pairs it added); no data is lost either
way, and promoted files simply stay.

Behaviour changes that follow: a file in a File field is now served to every
viewer of a published item that references it (before, only to its uploader
and administrators); a referenced file is promoted on every write path, so the
temporary file cleanup no longer deletes files attached through the item API, a
plugin or config import; a save that attaches a file the acting user cannot
fetch is refused with 403; uploads up to 10 MB work, and a larger one is a 413;
an upload whose filename extension disagrees with its content is refused with
415; `/files/` answers with the stored type, not one guessed from the path.

**How to find out.** Files the migration will make public:
`SELECT count(*) FROM file_managed f WHERE NOT EXISTS (SELECT 1 FROM
file_reference r WHERE r.file_id = f.id);` before and after shows how many
files gained a reference. If a proxy fronts the kernel, check its body limit on
`/file/upload` (see INSTALL.md, "Serving uploaded files").

### Plugins filter user HTML through `host::filter_text`; plugin API 0.105

**Who is affected:** a plugin that renders HTML a user wrote; anyone pinning
the plugin API version.

**What changed.** A new host interface, `text-format`, has one call,
`filter-text(text, format)` (SDK: `host::filter_text`), which runs the
kernel's own text format filter for a format the acting user may use, and
returns an error for any other, never the text unfiltered. A plugin that returns
user written HTML must filter it through this: the kernel does not filter a
plugin's output. Declare `text-format` in `host_interfaces`. The project
version moves to 0.105.0, so `KERNEL_API_VERSION` is `(0, 105)` and the bundled
manifests declare `api_version = "0.105"`. A plugin that calls `filter_text`
should declare `0.105`, which a 0.104 kernel refuses cleanly instead of failing
to link the import.

**How to find out.** `grep -rln "| safe\|text/html" plugins/*/src` lists
plugins that return HTML; check each one's user written parts.

### Form AJAX for every editor; conditional fields

**Who is affected:** a plugin implementing `tap_form_ajax`; a script or theme
calling `POST /system/ajax`; a site that wants a field shown only for some
values of another.

**What changed.** `/system/ajax` no longer requires `administer site`. It
requires an `X-CSRF-Token` header (the form prints one in `_ajax_token`, and
each response carries the next in the same header, since tokens are single
use), a form state built for the caller, and that the caller may still open
the form; otherwise 403. `tap_form_ajax` now runs with the full host API. "Add
another" on a multi value field is built by the server and works on
`/item/{id}/edit` for any editor. The old built-in `remove_{field}_{n}`
trigger and `templates/form/multi-value-item.html` are gone: clearing a value
removes it on save. A field definition may carry `settings.visible_when: {field,
equals}`; existing definitions load unchanged.

**How to find out.** `grep -rn "system/ajax\|tap_form_ajax" plugins themes`
lists callers to check for the header.

### Content and profile forms run the form taps; one form renderer

**Who is affected:** a plugin that implements `tap_form_alter`,
`tap_form_validate` or `tap_form_submit`; a theme that overrides
`templates/admin/content-form.html`, `templates/user/profile.html` or any
`templates/form/*.html`; a script that posts to `/item/add/{type}`,
`/item/{id}/edit` or `/user/profile` without loading the page.

**What changed.** The item forms (on `/item/...` and `/admin/content/...`) and
the profile form are now built as a kernel `Form` and rendered by the one form
renderer, so the three form taps fire for them, as documented in
`docs/plugin-development.md` (Forms). Before, none of them fired for any form a
route served, so a plugin's implementation of these taps starts running on
upgrade. Form ids are `item_add_{type}`, `item_edit_{type}` and
`user_profile_form`. Alterations chain in weight order. A plugin that adds a
button needing a script names a kernel asset library in `libraries` (the AI
Assist buttons use `ai_assist`); the admin content page no longer loads
`ai-assist.js` and `file-upload.js` on its own. The `/item/...` forms render
inside the site theme rather than as a bare page, and no form carries an inline
`<style>`; form styles are in `static/css/forms.css`.

`content-form.html` now prints only `form_html`; a theme override of it that
builds its own inputs must print `form_html` instead. Form element templates
read element properties directly (`element.options`, not
`element.element_type.options`). The item forms carry their CSRF token as
`_token` (was `_csrf`), and the profile form now needs `_form_build_id` too.

**How to find out.** `grep -rn "tap_form_" plugins/*/src` lists the plugins
whose form taps will start firing; `ls themes/*/templates/form
themes/*/templates/admin/content-form.html` lists overrides to review.

### Item form saves keep what the form did not show; Booleans are JSON booleans

**Who is affected:** a site whose items carry keys the edit form does not
render (plugin written keys, keys a type no longer declares, multi value
fields), a site whose Boolean fields were saved from an item form, and a
script that posts urlencoded bodies to `/item/add/{type}`, `/item/{id}/edit`
or the admin content forms without first loading the page.

**What changed.** A save from either item form now starts from the stored
fields and overwrites only the fields the form rendered, so nothing else is
erased. A checked Boolean is stored as JSON `true` and an unchecked one, when
the form showed it, as `false`; a migration rewrites the `"1"`, `"0"`, `"true"`
and `"on"` strings already stored in items for fields a type declares as
Boolean. Gather filters on a Boolean field now match items that were saved
from the admin form. Revisions are immutable history and keep the string they
were saved with, so reverting to a revision from before the upgrade stores the
string again until the item is next saved from a form. A field with a cardinality other than 1
is posted as `name[0]`, `name[1]` and stored as an array; more values than its
cardinality is a field error.

Each rendered form now saves server side state under its `_form_build_id`, and
a urlencoded submission must carry the build id of a form rendered for the same
user (and, on an edit, the same item). One that does not is refused with "This
form has expired" and nothing is saved. JSON submissions are unchanged: absent
top level keys keep their stored values, and a `fields` object replaces the
field map, with fields the user may not edit copied back.

**How to find out.** `SELECT type, count(*) FROM item WHERE fields::text ~ '"1"'
GROUP BY type` lists types that may hold Boolean strings before the upgrade; a
script posting item forms without a build id will start receiving 400s.

### Reading a stage needs `use stages`; gather totals count what the viewer can see

**Who is affected:** a site whose editors preview a stage other than Live, and
anything that reads a gather's `total` or pager for a signed-in viewer.

**What changed.** A new kernel permission, `use stages`, decides who may read a
stage other than Live: switch the session's stage at `/admin/stage/switch`, and
see it, with Live beneath it, in search, gathers (`?stage=`) and URL aliases.
No role is granted it on upgrade; administrators hold it implicitly. Before,
the switch was for administrators only, but a gather took `?stage=` from anyone.
If editors who are not administrators should preview stages, grant their role
`use stages` at `/admin/people/permissions` or in its role file. A request for
a stage without the permission is answered from Live, not refused, and a
switch to a stage that does not exist is now a 400.

A gather's `total`, `total_pages` and `has_next` now count only the rows the
viewer can see, up to the gather scan cap (`max_scan`, 1000 by default). For an
anonymous visitor nothing changes on Live; for a signed-in viewer who cannot
see some matching rows (unpublished items by others, say), the numbers go
down to what they can actually page through.

### New content lands on the default stage; stage moves follow the workflow

**Who is affected:** a site that marked a stage other than Live as the default
at `/admin/structure/stages`, a site with a `workflow.editorial` variable (the
tutorial ships one), and a plugin that calls `save-item`.

**What changed.** A create that names no stage used to land on Live whatever
the stage screen said. It now lands on the stage marked default. Find out
whether that is you before upgrading:

```sql
SELECT machine_name FROM stage_config WHERE is_default;
```

If that answers anything but `live`, new items (from the forms, the JSON API,
`save-item` and MCP) will start arriving there, and will not be public until
they are moved to Live.

The `workflow.editorial` variable is now enforced. A user moves an item from one
stage to another only when a transition names exactly that pair with a
permission they hold; with no workflow, only administrators move items.
`config import` now refuses a workflow transition that names a stage or a
permission it cannot find. The tutorial's file named `publish conferences`, a
permission the Part 5 plugin declares, and would be refused before that plugin
is enabled; it now names the kernel's `publish content`, which the tutorial's
publisher role holds. If your site copied the tutorial's file, check each
`permission:` against what your roles hold.

`save-item` now honours a `stage` machine name it used to ignore; a plugin that
sends one will see its writes moved, or refused with `ERR_ITEM_ACCESS_DENIED`.

An item whose stage row is missing (a `stage_config` row deleted by hand) is now
denied to everyone but administrators, where it was treated as visible.

### Login and registration admit their stated counts; rate limits by role

**Who is affected:** a site that tuned `login` or `register`
(`TROVATO_RATE_LIMIT_LOGIN`, `TROVATO_RATE_LIMIT_REGISTER`, or the
`rate_limit.login` / `rate_limit.register` site configuration keys) to what it
saw in practice, and a site that wants different limits for different roles.

**What changed.** Each sign-in and registration attempt used to be counted twice
against the same per-address budget, once by the rate-limit middleware and again
by the handler, so the configured count admitted about half as many attempts as
it said (`register`, 3 an hour, admitted one). Each attempt is now counted once,
so the stated count is what an address gets: twice what it effectively allowed
before. If you raised either count to compensate, lower it again.

New keys, nothing to do unless you want them: a bucket can give a role its own
count, `TROVATO_RATE_LIMIT_<BUCKET>__<ROLE>` or `rate_limit.<bucket>.<role>`
(the role's name lower-cased, other characters as underscores, so
`anonymous_user`, `authenticated_user`). An anonymous request gets the
`anonymous_user` count per address; a signed-in request gets the most generous
count among its user's roles per user. With no role keys set, every limit is what
it was. See "Rate Limits" in `INSTALL.md`.

### List every proxy hop in `TRUSTED_PROXIES`

**Who is affected:** a site behind more than one proxy (for example a CDN in
front of a load balancer) that sets `TRUSTED_PROXIES`. Find out by checking how
many proxies a request passes through before it reaches Trovato, and whether
`TRUSTED_PROXIES` names each of them.

**What changed.** The client address is now read from the right of
`X-Forwarded-For`: the first entry that is not a trusted proxy is the client.
The leftmost entry was used before, and a client can write whatever it likes
there. If only the proxy nearest Trovato is listed, the client address now
resolves to the next proxy out, and every visitor behind that proxy shares one
rate-limit bucket and one login lockout counter. List every proxy hop's address
in `TRUSTED_PROXIES`. A site behind a single proxy needs no change.

Related changes that need nothing from an operator:
- The login lockout now counts five failures per username and client address,
  plus fifty per username across all addresses.
- Password reset requests have their own limits: five per client address per
  fifteen minutes, and three mails per destination address per hour, the second
  overridable as `TROVATO_RATE_LIMIT_RESET_MAIL`.

### API token requests no longer receive a session cookie

**Who is affected:** any client that authenticates with an API token
(`Authorization: Bearer <token>`) and then relied on the session cookie the
response set, sending the cookie instead of the token on later requests. Find
out by checking whether the client stores cookies from API responses and sends
requests without the `Authorization` header afterwards.

**What changed.** A token request is now stateless. It is authenticated for that
request only: the response carries no `Set-Cookie`, nothing is stored in Redis,
and nothing is added to the user's list of signed-in devices. A client must send
its token on every request. A session the old behaviour created stays valid
until it idles out (24 hours without use), but no new ones are created; such a
session appears on its user's sessions page and can be ended there.

Two related changes need nothing from an operator: revoking a token takes effect
on the next request instead of up to a minute later, and a token-authenticated
write no longer needs a CSRF token.

### `item-api` acts as the requesting user, and needs a capability in the background

**Who is affected:** a plugin that imports the `item-api` interface —
`get-item`, `save-item`, `delete-item`, `query-items`. No in-tree plugin does;
the SDK has no binding for it, so this is plugins that hand-roll the imports,
which in practice means Argus and Netgrasp. Find out with
`wasm-tools print <plugin>.wasm | grep item-api`, or look for `item-api` in the
plugin's import section.

**What changed.** These four functions decided nothing. They called the `Item`
model directly and used the requesting user only as an author id, so a plugin
handling an anonymous visitor's request could read unpublished items and
restricted fields through it, and rewrite or delete any item by id.

They are decided as the requesting user now. A request-scoped call — any user
that is not the background principal, anonymous included — gets that user's own
answer: `get-item` requires `view` and drops fields the user may not see,
`query-items` returns only what the user may view, `save-item` requires `edit`
on an update and `create {type} content` on a create, and `delete-item`
requires `delete`. A denied read is indistinguishable from a missing item and
writes `null`, exactly as a missing id does. A denied write returns
`ERR_ITEM_ACCESS_DENIED` (-60).

Writes now go through the item service as well; see the next entry.

**Background contexts need a capability.** Cron and the queue worker run under
the kernel-internal background principal, which has no identity and no
permissions, so there is no user whose authority an `item-api` call could act
with. A plugin that calls these functions from `tap_cron` or `tap_queue_worker`
must declare the new manifest capability:

```toml
[capabilities]
item_background = true
```

With it, a background call behaves as it did before, with kernel authority.
Without it, every `item-api` function returns `ERR_ITEM_BACKGROUND_DENIED`
(-61). This is the same declared, auditable manifest plane `ai_background`
already uses, and for the same reason.

**What to do.** If your plugin writes items from a request path on a user's
behalf, check that the user it is acting for actually has the rights you were
relying on the kernel not to check. If it touches items from cron or the queue
worker, add `item_background = true` before you upgrade, or those jobs start
failing on the first run.

### A plugin's `save-item` and `delete-item` fire the item taps

**Who is affected:** a plugin that writes items through `item-api` (`save-item`,
`delete-item`), and a plugin that implements `tap_item_presave`,
`tap_item_insert`, `tap_item_update`, `tap_item_update_index` or
`tap_item_delete`. Find out with `wasm-tools print <plugin>.wasm | grep -E
'item-api|tap_item_'`.

**What changed.** A plugin's item write is now the same write a person makes:
it goes through the kernel's item service, so the item write taps fire on the
other plugins, the field and publish gates apply, a revision is recorded and the
item cache is refreshed at once instead of after `CACHE_TTL_ITEMS`. Three
consequences a plugin can see:

- A write that changes a field the acting user may not edit, or changes whether
  an item is published without `publish content`, is refused with
  `ERR_ITEM_ACCESS_DENIED` (-60). It used to be written.
- `delete-item` on an id that names no item returns `ERR_ITEM_NOT_FOUND` (-62)
  instead of `0`.
- A tap that writes items while handling an item write no longer runs alone: its
  write fires the taps of every plugin not already handling one around it, and a
  write nested four write taps deep is refused with `ERR_ITEM_WRITE_DEPTH` (-63).

`save-item` now also takes `language` on a create, which must be one of the
site's languages.

**What to do.** A plugin that implements an item write tap and writes items
from it should check it does not depend on its own write being invisible to the
other plugins. A plugin that treated `delete-item`'s `0` as "gone or never
there" should accept -62 as the second half of that. The SDK now binds all four
functions (`trovato_sdk::host::get_item`, `save_item`, `delete_item`,
`query_items`); hand-written `item-api` imports keep working.

### A plugin with an import the kernel does not provide fails to load

**Who is affected:** a plugin whose module imports a function the kernel does
not provide: a WASI function other than `fd_write`, `random_get`,
`environ_get`, `environ_sizes_get`, `proc_exit` and `clock_time_get`, or a
function missing from a host interface it declares. Find out by looking for a
`cannot be linked` error naming the plugin in the startup log.

**What changed.** Such a plugin used to load and report success, then fail every
tap at dispatch with `unknown import`. It now fails to load, with a message
naming the import, and the rest of the site starts without it. A plugin built
with Rust after 1.96 imports `clock_time_get`, which the kernel now provides,
so those plugins load and run.

**What to do.** Nothing, unless the startup log names one of your plugins; that
plugin was already failing on every request.

### A plugin call now has a wall clock ceiling

**Who is affected:** any site; in practice only a plugin that makes many slow
host calls in one invocation.

**What changed.** Guest CPU was already bounded by epoch interruption, and the
deadline callback extended that budget by however long a call had spent parked
inside host functions, so that waiting on an AI provider or a feed fetch was
not billed as computing. The extension had no ceiling, so a guest looping over
slow host calls was never interrupted and could hold a request open
indefinitely.

Total elapsed time per call is now bounded too, separately from CPU: 120
seconds for a request-scoped call and 900 seconds for a background one, both
far above the epoch budgets they sit beside, and both overridable with
`PLUGIN_REQUEST_WALLCLOCK_SECS` and `PLUGIN_BACKGROUND_WALLCLOCK_SECS`. A call
stopped by this bound is logged distinctly from one that exhausted its CPU, so
the two are separable.

### Plugin raw SQL is parsed, and some tables are off limits to every plugin

**Who is affected:** a site running a plugin that declares `raw_sql = true` or
names a kernel table in `db_tables`. Find out with
`SELECT name FROM plugin_status WHERE status = 1;` and read those plugins'
`.info.toml` files; a site running only the shipped plugins is affected in
exactly one place, described below.

**What changed.** Two things, both narrowing what a plugin's database access
reaches.

A protected list now sits under the structured table allowlist and is checked
before it, whatever a plugin's manifest or migrations say: `users`,
`user_roles`, `roles`, `role_permissions`, `plugin_permission`, `api_tokens`,
`password_reset_tokens`, `email_verification_tokens`, `recovery_codes`,
`recovery_email_challenges`, `webauthn_credentials`, `security_audit_log`,
`site_config`, `plugin_status`, `plugin_migration`, `_sqlx_migrations`,
`form_state_cache`, `user_tenant`, `oauth_client` and `webhook`. A call to one
of these is refused with the same `table-not-declared` ABI code an undeclared
table has always returned; the host log says `table-protected` so the two are
separable. Raw SQL is checked against the same list, which it never was before.

And raw statements are parsed rather than scanned for their first keyword.
`query-raw` takes a read and nothing else, and runs in a transaction the server
opens `READ ONLY`. `execute-raw` takes one INSERT, UPDATE or DELETE and nothing
else — narrower than before by `SET`, `RESET`, `DO`, `CALL`, `COPY`, `LOCK`,
`COMMENT`, transaction control, `VACUUM`, `REFRESH` and `DISCARD`. A statement
that does not parse is refused.

**`site_config` deserves its own line.** It holds the SMTP password. The
configuration form writes whatever an administrator types into the
`smtp_password` key, in the clear, unless they used the `env:` indirection, so
a plugin reading the table was reading a credential. If you have ever typed an
SMTP password into `/admin/config` rather than setting `SMTP_PASSWORD` in the
environment, that password is sitting in your database in plain text today;
protecting the table stops plugins reading it but does not remove it. Move it
to the environment and re-save the form.

**What to do.** Nothing, for the shipped plugins. Every raw statement they send
is pinned in a kernel test and still runs, with one exception: `trovato_ai`'s
field-rules read from `site_config` is refused now. That read was already
failing for its own reason — it filters on a column named `name` and the column
is `key` — so no behaviour a site relies on changes; the plugin logs the error
code and returns no rules, as it did before.

For a plugin of your own: if it reads a protected table through raw SQL, it
stops getting rows. If it uses `execute-raw` for anything but one row change,
it stops. Neither is recoverable by a manifest change, which is the point —
raw SQL remains a declared trust grant, but the grant no longer carries the
ability to write through the read path, run DDL through the write path, or
change the session every later kernel query on that pooled connection runs
under.

### The image no longer ships the `argus` plugin

**Who is affected:** a site that runs Argus from the published image, which is
what the old `argus` compose profile in this repository did. Find out with
`SELECT status FROM plugin_status WHERE name = 'argus';`: no row, or a status
of 0, means this is not you.

**What changed.** Argus moved to its own repository, `jeremyandrews/argus`. An
image built after this change has no `plugins/argus`, so a site with it enabled
starts without it: its routes, cron work and queue jobs stop, and its tables and
data stay exactly where they are.

**What to do.** Install it from its repository as an overlay: append its
plugin directory to `PLUGINS_DIR`, or use that repository's own
`docker-compose.yml`, which runs this kernel's published image unmodified. It is
the same plugin with the same migration files, so migrations the site has
already applied are not run again.

## v0.104.0 — 2026-09-24

Four queue and cron changes alter what a running site does with work it has
already claimed. None of them needs a migration, and none needs a configuration
change before you upgrade. Read the first two if you run plugin queues.

### A queue now drains at the width it declared, not at its plugin's widest

**Who is affected:** a site running a plugin that declares more than one queue
in `tap_queue_info` with different `concurrency` values. A plugin with a single
queue, or with the same width on all of them, sees no change.

**What changed.** The kernel read the **maximum** `concurrency` across a
plugin's queues, once per plugin, and applied it to a claim that named no queue.
A plugin declaring `analyze: 4, cluster: 1, summarize: 1` ran all three four
wide, out of four slots they shared. Each queue now claims within itself and
drains at its own declared width, still clamped to `QUEUE_CONCURRENCY_CAP`. A
queue holding rows the plugin never declared drains at width 1.

**What you will notice.** Throughput moves on both sides. A queue that was
inheriting a larger sibling's width slows to the width it asked for, and a queue
that was starving behind a sibling's stuck jobs now drains alongside them. If
you tuned a plugin's declarations while the maximum was the only number that
mattered, those declarations now mean what they say.

**What to do.** Nothing, unless a queue you relied on running wide was only
running wide by inheritance. `CronService::resolved_queue_widths` reports the
width the drain will honor for each queue, which is the answer that did not
exist before.

### Queue rows that have spent their attempts are retired on the first drain

**Who is affected:** a site whose `plugin_queue` table holds rows that were
claimed, passed `max_attempts`, and were never given a terminal outcome. If the
table has none, nothing happens.

**What changed.** `claim_batch` selected on `status`, `next_attempt_at` and
`locked_until` with no `attempts < max_attempts` term, so a row whose claimer
died after the bound was handed back to a worker forever, `attempts` climbing
past its own limit. The bound is now part of the eligibility predicate, and a
reaper at the head of every drain retires rows that are claimed, past the bound,
and past their lease.

**What you will notice.** On the first drain after the upgrade, those rows move
to dead with a `dead_reason` saying the claim lease expired with no terminal
outcome recorded. They are rows that were already past their limit; what changes
is that they now have an ending, and stop taking a worker slot on every cycle.

**What to do.** Count them first if you want to know the size of it:

```sql
SELECT plugin, queue, count(*)
FROM plugin_queue
WHERE status = 'claimed'
  AND attempts >= max_attempts
  AND locked_until < now()
GROUP BY plugin, queue;
```

Crash recovery is unchanged: a job with attempts still on it whose claimer died
is reclaimed exactly as before.

### A job that burns its whole CPU budget is dead-lettered on the first occurrence

**Who is affected:** a site with a plugin queue job that runs the background tap
epoch budget (`PLUGIN_BACKGROUND_TAP_DEADLINE_SECS`, default 150) to exhaustion.

**What changed.** Such a job was recorded as an ordinary failed attempt and
rescheduled, and the retry got the same budget to burn against the same wall. It
is now dead-lettered at once, whatever `attempts` says, with a reason naming the
budget.

The counterpart matters as much: the budget now counts only what the guest
executed. Time parked in a host call — an AI request, a feed fetch — no longer
counts against it, so a job that is merely waiting on a slow provider is not cut
off at all. The old wall-clock reckoning could not tell the two apart, and a
single slow response was enough to lose a job on its first attempt.

**What to do.** Nothing, unless you were relying on retries to carry a job that
genuinely computes past the budget. Raise `PLUGIN_BACKGROUND_TAP_DEADLINE_SECS`
for that deployment if so; it is settable per service, and its default is
unchanged.

### A cron run outlives the client that triggered it

**Who is affected:** a site that pokes `/cron` from a scheduler with a timeout
shorter than a run — the usual `curl -m 30` in a crontab.

**What changed.** The handler awaited the run inline, so a client hanging up
dropped the whole run mid-await: claimed rows were left with their tasks aborted
where they stood, the cron lock was never released, and a leaked heartbeat kept
renewing it until the process restarted. The route now runs cron on a task of
its own and awaits that, so a disconnect cancels only the waiting.

**What you will notice.** Your poker's timeout stops meaning what it used to
mean. `curl -m 30` still returns a timeout on a run that takes longer, but the
run now finishes, releases its lock and completes its queue bookkeeping instead
of dying there. A timeout from the poker is no longer evidence that the run
failed.

**What to do.** Nothing is required. If you alert on the poker's exit status,
that alert now reports "the run took longer than 30 seconds" rather than "the
run died", and should be read, or re-tuned, accordingly.

## v0.103.0 — 2026-09-21

### `/admin` takes a new `access administration pages` permission

**Who is affected:** a site that has delegated an admin screen to a role — one
holding `administer comments`, say — and wants that role to use the dashboard.
A site whose only administrators are superusers or roles holding `administer
site` needs to do nothing: the migration below covers it.

**What changed.** Every admin screen has its own permission, and until now
`/admin` itself took `administer site`. So a delegated role could reach the
screen it was given by typing the address, and got 403 on the dashboard that
would have linked to it. The delegation worked everywhere except at the front
door.

`/admin` now takes `access administration pages`. It is admission to the
administration section and confers no authority inside it: every screen still
asks for its own permission, and the dashboard shows only the links the viewer
may actually open.

**The migration.** `access administration pages` is granted once to every role
that already holds `administer site`, so no existing site loses its dashboard.
That is the only automatic grant. It is a one-time correction for a permission
that was split in two, not a rule: `administer site` does not imply admission
afterwards, and a role granted `administer site` from now on gets exactly that.

**What to do.** If you want a delegated role in the administration section, add
`access administration pages` to it, at `/admin/people/permissions` or in its
`role.*.yml`. This is new capability, not a regression: before this permission
existed there was no way to give that role the dashboard at all.

Superusers are unaffected.

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
