A gather feed's `ETag` now changes whenever anything the document prints
changes, not only when an entry itself is edited.

B127 hashed each entry's id, `changed` timestamp and link, so a repeat poll
could be answered `304` cheaply. What it left out was the entry's author name
and its taxonomy term labels: both are printed beside those hashed fields
(`dc:creator` / `<author><name>`, `<category>`), but both live on rows an
entry's own `changed` knows nothing about — `users.name`, `category_tag.label`.
Renaming an account or relabelling a tag left the old `ETag` matching, so a
conditional poll kept getting `304` for a document that now read differently,
and readers carried the stale name until the post's next unrelated edit.

The author's name is now hashed too — it was already fetched on the `304`
path for `dc:creator` itself, so this costs nothing new. Each term the entry
prints is hashed by id and label, and the labels are read in one batched query
for the whole response before the `304` check (never one per entry): that one
indexed lookup is the added cost of a poll that matches. The `Categories`
cache generation counter would have saved it, but it moves on any tag write
anywhere on the install, so while anyone edited tags no feed would ever answer
`304`; the labels move only the feeds that print them.

Also fixed: the `lakeside` fixture theme's `base.html` carried only the RSS
`<link rel="alternate">` for feed autodiscovery, missing the Atom sibling the
stock theme has advertised since B127.
