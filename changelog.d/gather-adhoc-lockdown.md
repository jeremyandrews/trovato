The ad hoc gather endpoint, `POST /api/gather/query`, now requires `administer
site`, and a gather definition can no longer read account or credential tables.
The endpoint had no permission check at all, so anyone could post a definition,
and gather validation checked only that table and column names were well formed,
never which tables or columns a definition was allowed to read. A join from
content to an account table therefore projected that table's columns straight
into the response. The per-row access check looks only at the base content item
and never saw them.

The policy now lives in `GatherService::validate_definition`, which every
execution path runs through, so it holds for named gathers, includes, and the
definitions plugin migrations and config imports write directly to
`gather_query`, not just for the one route. A base table must be `item` or a
registered record type. A join may only reach a short allowlist of content
tables. Account, credential, token, permission and secret tables are refused
outright, and so are columns named like secrets. `register_query` and config
import now refuse an invalid definition when it is saved, naming the rule it
broke. The unused `core.user_list` default view, which read the `users` table, is
no longer registered.
