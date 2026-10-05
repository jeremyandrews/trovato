Raw SQL from a plugin is parsed now, and there is a floor under the table
allowlist.

The allowlist a plugin's structured `db` calls are confined to was built from
the tables its own migrations create, unioned with the `db_tables` list in its
manifest. Both halves are written by the plugin, so the allowlist was a request
rather than a limit: a manifest saying `db_tables = ["users"]` put the users
table in reach of a structured `select`, and a migration reading
`CREATE TABLE IF NOT EXISTS users` succeeds silently against the table the
kernel created at install and was then taken as owning it. A protected list now
sits underneath, checked before the allowlist and refusing whatever the plugin
declares: the credential and authorization tables, the tokens of every kind,
passkeys, the audit trail that would record tampering with them, the tables
deciding which plugins run and which migrations have been applied, the
form-state cache, the tenant grant, and `oauth_client` and `webhook`, which
hold secrets of their own.

`site_config` is on the list too, and that one changes behaviour. The table
holds the SMTP password: the configuration form writes whatever the
administrator types into the `smtp_password` key, in the clear, unless they
used the `env:` indirection. A single `SELECT value FROM site_config` was
therefore a credential read available to any plugin holding `raw_sql`.
`trovato_ai` is the one shipped plugin that reads the table and is refused now;
it already handled the error by logging it and returning no field rules, and
the query had in any case been failing on its own since it was written, because
it filters on a column named `name` and the column is `key`.

Raw SQL was judged by its first keyword, which cannot see two things. A
statement can be a read at the front and a write inside, so
`WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x` begins with `WITH` and
passed the read-only check while PostgreSQL ran the delete. And the first
keyword is only the first keyword if you skip comments the way the server does:
PostgreSQL nests block comments, the scanner stopped at the first `*/`, so in
`/* /* */ SELECT 1 */ DROP TABLE t` it read `SELECT` where the server read
`DROP TABLE`. Statements are parsed with the PostgreSQL dialect now and judged
as a tree. `query-raw` takes a read and nothing else; `execute-raw` takes one
INSERT, UPDATE or DELETE and nothing else, which is narrower than the old rule
of "anything whose first word is not DDL" by `SET`, `RESET`, `DO`, `CALL`,
`COPY`, `LOCK`, `COMMENT`, transaction control, `VACUUM`, `REFRESH` and
`DISCARD`. A statement that does not parse is refused rather than handed to the
server to interpret.

`SET` mattered more than the rest of that list. The host runs an `execute-raw`
statement inside `BEGIN` and `COMMIT` on a pooled connection, and a plain `SET`
is not scoped to the transaction, so one call changed the session every later
kernel query on that connection ran under. `SELECT set_config(..., false)` did
the same thing through the read path, so the function denylist closes that one
and, with it, the session-level advisory locks, `dblink`, the large-object
functions, the server-file readers, the backend and log controls, and the
`*_to_xml` family, which takes a table or a query by name and would walk around
the relation check. `pg_sleep` stays allowed.

Both raw paths also walk the relations the statement names and refuse a
protected table anywhere in the tree, including inside a CTE and inside a
scalar subquery, which is where a table name hides best.

The control that does not depend on any of this being right is the transaction.
`query-raw` runs in one the server opens `READ ONLY`, from its first statement
rather than after some first statement has already run, so a write the parser
somehow blessed is refused by PostgreSQL. `do_insert` still needs a read-write
transaction for its `RETURNING *`, so the access mode is a parameter of the
shared row-fetching path rather than a property of it.

Every raw statement the shipped plugins and the two test fixtures send is
pinned in a test, copied verbatim, and accepted by the function that sends it;
no plugin source changed. Raw SQL is still a declared trust grant and a plugin
holding `raw_sql` can still read any table this floor does not protect. What
closed are the paths that ran past that grant. A per-plugin database role, with
its own grants, remains the stronger answer and stays a post-1.0 option.
