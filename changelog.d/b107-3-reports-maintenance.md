`/admin/reports/cron` shows the last cron run: when it ran, on which server,
its result, how long it took and which tasks did work. A Run now button runs
cron in the server's own process, through the forms pipeline with its CSRF
token, and the report then shows that run. It takes `administer site`, as
`/cron/status` does. The last-run record gains `status`, `tasks` and
`duration_ms`; a record written by an older server still shows its result.

`/admin/reports/log` is the site log. A `tracing` layer writes warnings and
errors to a new `system_log` table, along with three kinds of `info` event:
sign-ins (success, failure and lockout, with the username), plugins enabled or
disabled (from the admin screen or the CLI), and config imports. Other `info`
events stay in the server log only. A field whose name looks like a secret
(passwords, tokens, keys, cookies, session ids, CSRF tokens, request bodies) is
never stored. Writes go through a bounded channel and are batched, so logging
never waits on the database. Cron prunes the table to whichever is smaller:
`SYSTEM_LOG_RETENTION_DAYS` (default 30) days or `SYSTEM_LOG_MAX_ROWS`
(default 100,000) rows. The screen pages 50 entries at a time and filters by
level and type. It takes `administer site`.

Maintenance mode is a setting on `/admin/config/site`, with an optional message.
While it is on, every request gets a 503 with the themed `maintenance.html`
page, `Retry-After: 300` and `Cache-Control: no-store`. The exceptions are a
signed-in user holding the new `bypass maintenance mode` permission
(administrators hold it), and `/user/login`, `/static/*`, `/health` and
`/cron/*`, so an administrator can still sign in and cron still runs. The
setting is read on each request, so every server in a cluster turns away
visitors as soon as it is saved. A site theme can override `maintenance.html`.
It extends `base.html` and receives `site_name` and `maintenance_message`.
