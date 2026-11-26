The accessibility report only ever looked at uploaded files: it listed an
image with no text alternative set on its file record, but an `<img>`
written straight into a rich text field's HTML was invisible to it, so a
site could carry many such images and the report would still say none
existed.

**In content** (`/admin/reports/accessibility`, under Reports). A new
section lists content items whose rich text holds an `<img>` with no `alt`
attribute, or one that is only whitespace (`alt=""` is a deliberate
decorative marker and is not listed), with the total and the newest fifty,
each as a title, a type and a count, linking to the item's edit form for
someone who may edit it and to the item otherwise.

The count is computed when an item is saved, using the same HTML parser the
static search index already tokenizes pages with, and stored on the item;
the report only ever reads the stored count, so a large site's page load
never parses every item's HTML. Existing items are caught up over a few
cron runs.
