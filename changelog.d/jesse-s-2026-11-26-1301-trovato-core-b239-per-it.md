Comments were a per content type setting only (B159): closing them for one
post meant closing them for every post of that type, and a site migrated from
Drupal 6, whose `node.comment` was per node, had nowhere to put the value it
imported. An item now carries its own override, `item.comment_mode`: `inherit`
(the default; every existing item) defers to its type's setting, and `open`,
`closed` or `hidden` win over it for that item alone. One function,
`content::comment_settings::effective_mode`, resolves the item then the type,
and every comment surface reads through it: the thread, every write route (the
form and the JSON API, replies included), the comment listing API and the
search index.

The item form offers a "Comments" control to a holder of the new permission
`set item comment status`, granted to no role by migration; an administrator
holds it implicitly. A plugin reads and sets the override through the
existing `get-item` and `save-item` host calls, so an importer built on them
can carry a migrated site's per node setting across: Drupal 6's `node.comment`
0 (disabled) maps to `hidden`, 1 (read only) to `closed`, 2 (read and write)
to `open`.

Not revisioned, the way `promote` and `sticky` are not: it is an
administrative flag beside the item, not part of what the item says.
