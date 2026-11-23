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
path for `dc:creator` itself, so this costs nothing new. Terms are covered
without bringing back a per-poll query against `category_tag`: each entry's
term ids are hashed (free, already parsed off the row) together with the
`Categories` cache generation counter the kernel already bumps on every tag or
category write (`crate::cache::generation`). That counter is one cheap Redis
read, shared by the whole response rather than queried per entry, so the only
added cost on the `304` path is that one read — exact in that a relabel is
never missed, at the price of invalidating every feed's `ETag` on any tag or
category write, not only ones naming a term the response actually carries.

Also fixed: the `lakeside` fixture theme's `base.html` carried only the RSS
`<link rel="alternate">` for feed autodiscovery, missing the Atom sibling the
stock theme has advertised since B127.
