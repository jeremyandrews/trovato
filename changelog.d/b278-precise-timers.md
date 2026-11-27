Timers: kernel code and plugins can now set work for an exact second, once or
repeating at a fixed interval, and it runs at that second on exactly one server.
Scheduled publishing uses them, so an item with a publish date goes live within
a second of it instead of at the next cron run, up to five minutes later.

Cron was the only way to run work later, and cron runs on an interval: whatever
fell due between two runs waited for the second one, and a plugin that needed a
precise time had to poll for it. A timer is a row in `scheduled_timer` with an
owner, a name, a next run and an optional repeat. Every server runs a scheduler
that sleeps until the earliest timer, wakes on a Postgres `NOTIFY` when one is
set earlier, and otherwise reads the table once a minute; nothing runs while
nothing is due. A due timer is claimed with `FOR UPDATE SKIP LOCKED` and a claim
token, so two servers never both fire it and a run's result is recorded only if
the timer was not replaced, paused or cancelled meanwhile. A server that dies
mid-run leaves a claim that lapses and is taken by another; one shutting down
gives back at once the claims it has not started. A timer set on a server wakes
that server's own scheduler directly as well as the others through `NOTIFY`. A repeating timer
keeps its cadence from its first slot and fires once after an outage, not once
per missed slot. A failure is retried twice with backoff, then the timer is
`failed` with its error.

Plugins declare the new `timer` host interface (`set`, `cancel`, `get`, only
ever on their own timers) and implement `tap_timer`. Four throttles bound them:
timers firing at once per server, timers per plugin, the shortest repeat and the
largest payload (at most 64 KiB), each refused with its own error code (-95 to -98) and a log
line naming the limit; a plugin over a lowered limit can still move the timers
it holds. `trovato_scheduled_publishing` sets a timer for each
publish and unpublish date when an item is saved, moves it when the date
changes and cancels it when the date is cleared or the item deleted. A publish
that fails to save keeps its date and is tried again. The plugin's
`tap_cron` sweep stays as the backstop for anything a timer did not handle.

`/admin/reports/timers` lists every timer with its next run in the viewer's
time zone, filters by owner and status, and, with the new `administer timers`
permission, pauses, resumes, runs now, cancels and edits them, and sets the
throttles (`TIMER_*` environment variables override those). `view timers` opens
the list alone. Every change is recorded in the security log. Both permissions
are granted to roles that hold `administer site`.
