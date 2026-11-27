A freshly installed site now runs its own cron. `CronService::run` used to be
called only from `POST /cron/{key}` and the admin "Run now" button, so a site
with no external caller never published scheduled content, never rebuilt its
search index and never cleaned up temporary files or expired sessions, and
every real deployment carried a sidecar container to poke the URL on a timer.
The kernel now spawns a scheduler at boot that calls the same `run` on its own
cadence: `cron_interval_secs` in `site_config`, default 300 seconds, edited on
the cron report (`/admin/reports/cron`), which now also shows whether the
scheduler is on and its interval. `0` turns it off. The `CRON_INTERVAL_SECS`
environment variable overrides the setting, as `UPDATE_CHECK` does for the
update check. The setting is reread at least once a minute, so a change applies
without a restart.

The scheduler shares the lock every other caller takes, so an external caller
stays harmless: whichever arrives second is told another instance is running
cron. It also waits out a run that finished less than an interval ago, whoever
ran it, so several servers, or a server plus an external poker, run cron about
once per interval together rather than once each. `/cron/{key}` is unchanged.

The lock itself got stricter. Its owner value was `host:pid`, the same for
every run in one process, so a run that outlived its lock could, on finishing,
release the lock a newer run in the same process had since taken, and let a
third caller start alongside it. Each run now writes a value of its own.
