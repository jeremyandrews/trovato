# Upgrading

What an operator has to know or do before moving a running site to a newer
version. Only versions that need something are listed; a version that is not
here needs nothing beyond the ordinary upgrade.

Each entry says what changed, who it affects, and how to find out whether that
is you **before** you upgrade.

## Unreleased

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

Writes still go straight to the model, so the insert, update and delete taps
still do not fire from `item-api`. That is deliberate and unchanged: dispatching
a tap from inside a tap is the re-entrancy this module exists to avoid. Only the
access decision is new.

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
