The revision history of an item says which revision is current, deletes a
past revision, and opens to a user who holds `view revisions` without being
able to edit the item. The list marked the current row only with a small
"current" badge, offered no delete at all, and asked for edit access before
showing anything, so a role that Drupal lets read the history (`view
revisions`) was refused with 403 when it edited the item through a plugin's
own form or not at all.

Two kernel permissions are new, both with Drupal's names so a migrated role
keeps them, and granted by no migration: `view revisions` reads the history,
a past revision and a comparison of two for anyone who may view the item, and
`delete revisions` deletes a past revision for a user who may also delete the
item (`POST /item/{id}/revisions/{rev}/delete`, answered 409 for the current
revision, which is never deleted). `ItemService::delete_revision` is the one
path, and records an audit row. Revert is unchanged: it is an edit of the item,
so it needs edit access and is offered only to a user who has it.
