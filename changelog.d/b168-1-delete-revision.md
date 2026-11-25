Deleting a past revision of an item (B158's `delete_revision`) is now reachable
from a confirm page, not just a one-click form on the revision list: the
revision list's Delete link opens `/item/{id}/revisions/{rev}/delete` (GET),
which asks the same `delete revisions` and delete-access questions the POST
does and answers them the same way, before it offers the button. An operator
reaches the same path from the command line, `trovato item revision-delete
<item> <revision>`, with the administrator bypass every other `trovato`
command already runs with.

The current revision was already refused by `ItemService::delete_revision`
(409); it turns out the database already refused it too, independently, since
the item table's original migration: `item.current_revision_id` is a foreign
key into `item_revision` with no `ON DELETE` action, so a `DELETE` reaching
Postgres by any other path — a script, a future bypass of the service — fails
with a foreign key violation rather than orphaning the item.

A delete also bumps the item's `Items` cache generation, which it did not
before: nothing served a stale revision list from it (revisions are not
cached), but `item_revision` is part of what that generation covers, and every
other write to an item or its revisions already bumps it.
