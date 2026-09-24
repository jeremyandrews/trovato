# Upgrading

What an operator has to know or do before moving a running site to a newer
version. Only versions that need something are listed; a version that is not
here needs nothing beyond the ordinary upgrade.

Each entry says what changed, who it affects, and how to find out whether that
is you **before** you upgrade.

## Unreleased

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
