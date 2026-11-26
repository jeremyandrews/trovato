An administrator can clear every cache, run cron and read a status report
from the admin, without a shell.

Clearing the caches was `trovato cache clear` and nothing else: the admin had
no control for it, so a change made behind the servers' backs (in the
database, by a script) stayed invisible until each server's copy expired, and
a site run by someone without shell access had no way to force it.
`/admin/config/performance` now has a "Clear all caches" button that runs the
command's one path, `cache::clear_everywhere` over every cache kind, so every
running server drops its copies before the page comes back with "Caches
cleared.". If Redis cannot be reached it clears this server's own copies and
says the others keep theirs.

There was no status report: the last cron run and the Run now button were on
the cron report, and the versions, database, cache server, uploads directory
and security settings were nowhere in the admin. `/admin/reports/status`
checks each when the page is asked for: the Trovato version and whether a
newer or security release exists, the PostgreSQL version and the schema's last
migration, the Redis version, when cron last ran (with a link to run it
manually), whether this server can write to the uploads directory, whether
session cookies are HTTPS only, and maintenance mode. Each row says OK,
Warning or Error in words. No connection string, key or password is read into
the page.

Both actions are POSTs through the forms pipeline, so each needs the page's
single use token, and each has its own permission: `clear caches` and
`run cron`. `administer site` still opens both, so no role loses what it could
do; the status report takes `administer site`.
