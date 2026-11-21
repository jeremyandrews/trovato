A plugin can run a Gather definition now, through a new `query` host interface
(`run`, bound in the SDK as `trovato_sdk::host::query`). Until now a plugin
that listed content across types, sorted and paged with each row's URL, had
to reach for `raw_sql`, which reads every unprotected table with no viewer,
stage or language. The kernel's own reference site did exactly that. The definition
runs through the same pipeline as a site's gather page, for the user the plugin
is acting for, on the request's read stages and in its language. The plugin
cannot set any of the three: `stage_aware = false` on an item query is refused.
Item rows carry `url`, and a filter on `url` finds an item by the address it is
served at, resolved for any gather. A plugin's own tables are queried with no
viewer semantics, an item query may join them, and they may not join `item`.
Tables that are not content must be in the plugin's allowlist. Includes and
`semantic_similarity` are refused. A page is at most 100 rows, a definition at
most 32 filters, 8 sorts and 3 relationships, and the statement timeout is 5
seconds. Two new codes: `ERR_QUERY_INVALID` (-80) and `ERR_QUERY_BOUND` (-81).

`query-items` listed items on every stage, so a live request could return a
published item from an `accessible` stage, which is never listed, and an editor's
plugin saw an internal stage's items in a live request. It now lists the
request's read stages only (Live outside a plugin route), newest change first
with the id breaking ties (BL-64). A plugin route now dispatches with the
viewer's read stages and negotiated language.

Contract: the `query` interface, its SDK binding, `QueryResult` and the two
error codes are **additive** (a new interface under the version map). What
`query-items` returns changes, so this is a **behavior change** to an existing
host function, which is frozen surface, taken in a minor release while the
project is 0.x.
