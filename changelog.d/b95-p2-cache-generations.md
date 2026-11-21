Every in-process cache that holds data from Postgres is now current on every
server within about a second of a write, on one server or many, with nothing to
configure.

Each such cache is a kind (permissions, users, items, field access, content
types, categories, gather queries, redirects, locale strings and config menus).
A write clears its own server's copy as before and then adds one to that kind's
counter in the Redis hash `cache:gen`. Every server reads the hash every 250 ms
and clears a kind whose counter changed since its last read; a difference, not
an increase, so a restarted Redis (which empties the hash) clears everything
once. A whole kind clears on a bump. When Redis cannot be read the server logs
once and the caches' existing TTLs bound how stale they get. A role revoked or a
user blocked on one server used to keep working on another for up to the
permission or user cache TTL.

Writes that skipped their service's invalidation now bump too: a plugin's
writes through the database host functions (by the table they name, so the
scheduled publishing plugin's raw `UPDATE item` reaches the item cache), a user
deletion (its items change author), a role rename, search reindexing and its
queued cron task, AI budget overrides, stage writes, a stage publish, and a
gather query's canonical URL, whose redirect is now written when the query is
saved rather than at the next periodic reload. `config import`, the plugin
commands and `user role-add`, `role-remove` and `reset-password` bump every
kind they could have touched, so `config import` no longer asks for a restart;
it says so only when it changed which plugins are enabled, which still waits
for one.

Two caches are gone rather than wired. The cache layer's in-process tier is
removed, so `get` and `set` go straight to Redis and a tag invalidated by one
server is invalidated for all; the `layer_l1` cache metrics are gone and
`CacheStats` reports the Redis key count. The stage cache and stage view are
removed too: stages and the workflow are read from Postgres (two small
queries), and `clear_stage_cache` is gone, along with the `item_stage` and
`stage_view` metrics. Locale strings now reload in place on a bump, so an
imported translation appears without a restart. Content types and gather
queries deleted elsewhere now leave the other servers on reload.

The session store's Redis pool had no reconnect policy, so after Redis
restarted every request that touched a session hung for good. It now
reconnects every half second and gives up on a command after five seconds.
