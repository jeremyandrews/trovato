Two operator commands. `trovato user create <name> --mail <address>
[--role <role>]... [--blocked] [--no-password]` creates an account with the
admin "Add user" form's checks and through the same user service, so plugins
see `tap_user_register` and the audit trail records the account and each role
grant; the password is read from stdin, and `--no-password` leaves an account
that cannot sign in until a reset. A taken or missing name or address exits 1
and creates nothing. `trovato cache clear [--kind <kind>]...` tells every
running server to drop its in-memory caches, so a change written straight to
the database is served on the next request rather than after the cache's time
limit or a restart. Sites importing accounts had been inserting into `users`
by hand, and sites writing by hand had been restarting their servers.

`cache clear` is the one invalidation path of the command line: `config
import`, `generate` and the `plugin` and `user` commands that write now end with
the same function rather than their own bump. Clearing `site_config` also drops
the search settings' copy in Redis, the one value read from `site_config` that
no cache generation reached, so a search settings change made by import or by
hand is no longer served stale for up to five minutes. A new account created
with an empty password now stores no password hash instead of the hash of the
empty string; no existing path created one.
