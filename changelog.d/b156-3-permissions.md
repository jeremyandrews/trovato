An author can now do on Trovato's admin screens what Drupal's finer
permissions let an author do there.

`access content overview`, Drupal's permission of that name, opens
`/admin/content` without `edit any content`. The list then holds the items the
viewer may see, and offers Edit and Delete only where the item allows them.
Its bulk actions are only the ones the viewer holds, and there are none for a
viewer who may change nothing.
The list used to need `edit any content`, so an author was refused it
outright.

The "Add content" chooser opens to whoever may create some type, and lists
those types. It used to ask for the generic `create content`, so an author who
held `create blog content` was refused the chooser for the form they could
use.

`access site reports` opens the site log without `administer site`.

`delete own content` deletes one's own items from the content list, one at a
time or in bulk. Before, the route asked for `delete any content`, so the
author had no way to delete their own item. Every item is still checked by
its own access check: someone else's item in an author's selection is refused
and left as it was. A refused single delete now answers `403`; it used to be a
failed write.

`create category terms`, `edit category terms` and `delete category terms`
open the matching term screens without `administer categories`. Drupal's
per-vocabulary "create terms in X" is imported as these, and before they
opened nothing. They reach the site's own vocabularies; a plugin's vocabulary
stays with its owner's route and with `administer categories`. So do the
workflow stages, which are tags too: they stay with the stage screens. Adding, editing
and deleting a vocabulary itself still takes `administer categories`.

The admin sidebar shows each of these screens to whoever its page now admits.

The content list also renders with the viewer it has already loaded, rather
than loading the signed-in user a second time: one query fewer per page (18
cold, 7 warm, for an administrator).
