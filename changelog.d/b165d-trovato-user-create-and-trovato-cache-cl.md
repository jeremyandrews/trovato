Two operator commands. `trovato user create <name> --mail <address>
[--role <role>]... [--blocked] [--no-password]` creates an account with the
admin "Add user" form's checks and through the same user service, so plugins
see `tap_user_register` and the audit trail records the account and each role
grant. The password is read from stdin, with echo off at a terminal, and
`--no-password` leaves an account that cannot sign in until a reset. A taken or
empty name or address exits 1 and creates nothing; a role that cannot be
granted once the account exists leaves the others granted, exits 1 and prints
the `role-add` commands that finish it. `trovato cache clear [--kind <kind>]...`
tells every running server to drop its in-memory caches and returns once they
have read the bump, so a change written straight to the database is served on
the next request rather than after the cache's time limit or a restart. Sites
importing accounts had been inserting into `users` by hand, and sites writing
by hand had been restarting their servers.

`cache clear` is the one invalidation path of the command line: `config
import`, `generate` and the `plugin` and `user` commands that write now end with
the same function rather than their own bump, and so also return only once the
servers have cleared. Clearing `site_config` also drops the search settings'
copy in Redis, the one value read from `site_config` that no cache generation
reached, so a search settings change made by import or by hand is no longer
served stale for up to five minutes. A new account created with an empty
password now stores no password hash instead of the hash of the empty string;
no existing path created one. `user reset-password` now reads a password typed
at a terminal with echo off too, where it used to show it as it was typed.
