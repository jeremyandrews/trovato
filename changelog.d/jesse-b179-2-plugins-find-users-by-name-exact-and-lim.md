A plugin can now resolve a user name to the same public account record
`load_users` already answers for an id: `host::find_user_by_name` for an
exact, case-insensitive match (the same match login makes), and
`host::find_users_by_name_prefix` for a short, sorted list an autocomplete
widget can offer as a person types. Both need the `user_directory` capability
`load_users` already needs, and neither ever answers an address, a password
hash or a user's `data`.

Before this, a plugin's own "add member by name" form could only match names
the plugin had already seen go through its own tables: `users` is a protected
table no plugin's raw SQL can read, correctly, since it holds hashes and
emails, but that also meant an account created straight on the site, never
mentioned to the plugin by id, answered "no such user" to a search by the
name right there on the account page. The new calls close that gap at the
kernel boundary instead of leaving every plugin to work around it.

Neither call filters a blocked account or the anonymous account out of its
answer — the same visibility rule `load_users` already has, and the one the
kernel's own `/admin/people` listing uses for the capability's calling
context: nothing beyond the capability gate itself.

The exact match reuses the unique index login already matches names against
(`idx_users_name_unique`, on `LOWER(name)`). The prefix match needed a new
one: under the official Postgres image's default `en_US.utf8` locale, neither
that index nor a plain `ILIKE` can turn a prefix search into a range scan, so
it fell back to a sequential scan of the whole table (180ms against 200k
seeded rows in testing). `idx_users_name_prefix`, a `text_pattern_ops` index
on `LOWER(name)`, answers the same query in under 1ms regardless of the
database's locale. The prefix itself is escaped before it reaches the query,
so a literal `_` or `%` a person typed matches itself rather than acting as a
wildcard.
