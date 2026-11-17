A gather can now require a permission, and no path serves its rows to a viewer
without it. Gathers had no access gate of their own: the gather page, the
`display.routes` aliases, the feed and the JSON API ran the query for anyone, and
item access, which filters rows one by one, does nothing for a gather that reads
a lightweight-record table. So a plugin gather over its own tables, such as a
list of network devices, was readable by an anonymous visitor at its route alias.
A definition may now carry `"access": { "permission": "..." }`, stored with the
definition and carried through config export and import. Every entry point
checks it with one function, `gather::authorize_gather`, before the query runs:
an anonymous visitor on an HTML path is sent to log in and a signed-in one gets
403, a JSON path answers 403 with a JSON error, a feed answers 403,
`/api/queries` leaves the gather out, and the MCP `run_gather` tool refuses it.
Administrators pass. Rows of a gather the viewer may run are still filtered by
item access.

A lightweight-record gather that declares no permission now requires
`access content`, the permission the published path of item access already asks
for, so it is no longer public by accident and a site that withdraws
`access content` from a role withdraws both kinds of listing. Item gathers are
unchanged. With the default role grants, where the anonymous role holds
`access content`, no page that answered before answers differently; on a site
whose anonymous role lacks it, a record gather now sends anonymous visitors to
log in.

`menu_link` rows are no longer drawn for a viewer who could not follow them. A
link whose address serves a gather the viewer may not run (directly, through a
URL alias, a canonical URL or a route alias) or a plugin route whose `tap_menu`
permission they lack is dropped, with every link under it. A site whose main menu
comes from `menu_link` keeps drawing that menu, even when a viewer may follow
none of its links, instead of falling back to the plugin menu. The gather builder
keeps a definition's `access` when the gather is edited and saved.
