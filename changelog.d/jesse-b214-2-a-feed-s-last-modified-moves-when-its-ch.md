A feed's `Last-Modified` tracked only its entries, so editing the channel's
title, description, or the alias its link resolved through left the header
where it was. A reader polling with `If-Modified-Since` alone (no
`If-None-Match`, which already caught this through the `ETag`) kept getting
`304 Not Modified` and the old channel values after such an edit.

`Last-Modified` is now the latest of the newest entry's `changed`, the gather
query's own `changed` (already covering a title or description edit, since
those live in the feed's config), and, when the channel link resolves through
a `url_alias` row instead of a fixed path, that row's own `changed`. The alias
had no reliable change time before this: editing one rewrites its row in
place, leaving `created` as it was. `url_alias` gets a `changed` column, set on
insert and on every update, backfilled from `created` for existing rows.
