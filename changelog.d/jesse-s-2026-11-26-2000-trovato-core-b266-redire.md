A redirect written from the admin form or the JSON API left other servers
serving the old destination until the redirect cache's TTL expired, and left no
audit trail entry, and could be saved pointing in a circle (A to B, B back to
A, or a redirect to itself) with nothing to stop it.

The root cause: `Redirect::create`, `update` and `delete` were bare database
writes, called directly by the admin routes and the API, with nothing between
a caller and the table. `Redirect::detect_loop` already existed but nothing
called it; nothing bumped `CacheKind::Redirects` outside gather's canonical
redirect sync and the B167 importer; nothing wrote to the audit log the way
item and user writes do.

Every redirect write now goes through one new `RedirectService`, held on
`AppState` the way `UrlAliasService` is: it refuses a create or update that
would complete a loop, naming the loop in its error; it clears this server's
redirect cache and bumps `CacheKind::Redirects` so every other server drops its
copy; and it records a `create`, `update` or `delete` row in the audit log. The
admin form and the JSON API both call it, and neither can skip any of the
three. The B167 importer is unchanged: it keeps the id and date a record gives
it and writes inside its own transaction, which the service's `create` cannot
do, and it already bumps the cache once for the whole run rather than once per
record.
