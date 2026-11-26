A freshly installed site now runs its own cron. Before this, `CronService::run`
only ever executed from `POST /cron/{key}` or the admin "Run now" button, so a
site with no external scheduler configured silently never published scheduled
content, never rebuilt its search index, and never cleaned up temporary files
or expired sessions — every real deployment carried a sidecar container just to
poke the URL on a timer.

A built-in scheduler now calls `run` on its own cadence from inside the kernel
process, so core is fully functional with nothing external configured. The
interval lives in `site_config` under `cron_interval_secs`, editable on the
cron report (`/admin/reports/cron`), which now also shows whether the
scheduler is on and how often it runs; `CRON_SCHEDULER_INTERVAL_SECS` overrides
it at the process level, the same precedence `UPDATE_CHECK` gives the
environment over a site setting. The default is 300 seconds, so a fresh install
runs scheduled cron immediately; `0` turns the scheduler off.

The scheduler calls the exact same `run` that `/cron/{key}` and the admin
button always have, so it shares the same Redis lock: a scheduled run and an
external poke landing in the same window never both execute, the second is
told another instance already is. An external scheduler stays harmless to
configure alongside the built-in one, and `/cron/{key}` is otherwise unchanged,
so an existing deployment's cron poker keeps working exactly as before.
