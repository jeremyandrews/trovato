Batches now run, and plugins can add bulk actions. Nothing ever executed a
batch: `/api/batch` recorded any `operation_type` and left it pending, while the
content list's bulk form did its work inline from a hard-coded list of three
actions, so a plugin had no way to offer one and a large selection had no
progress or way to stop.

One registry, `BulkActionRegistry`, holds the kernel's `publish`, `unpublish`
and `delete` (with the permissions the form already required) and every action
an enabled plugin declares under `[[bulk_actions]]` in its manifest, registered
as `"<plugin>.<name>"` and offered only to users holding its declared
permissions; a declaration with no permission, or from a plugin without the new
`tap_bulk_action`, is refused at manifest parse. A submitted action is a batch
plus a kernel job under `__kernel_batch`, drained by cron, the resident runner
and a task spawned at submit time. The executor works in chunks of 100: before
each it re-reads the batch, so a cancel stops it, and re-resolves the submitting
user's permissions from the database, so a revoked user stops it; kernel actions
make one `ItemService` call per item as that user, and plugin actions load the
chunk in one query and hand `tap_bulk_action` only the items the user may edit,
in a request carrying that user. Progress is recorded after every chunk and the
result carries the message the form has always shown.

`/api/batch` refuses an unregistered `operation_type` with 400 and an
unpermitted one with 403. The content list builds its select from the registry,
can apply an action to every item matching its filters, finishes a selection of
100 or fewer within the request as before, and runs a larger one in the
background, polling `GET /api/batch/{id}` from `static/js/bulk-progress.js`.

Contract: additive (`tap_bulk_action`, the `[[bulk_actions]]` manifest key, the
executor), classified with BL-102. `/api/batch` refusing what nothing would ever
have run is the one behaviour change on an existing route; UPGRADING describes
it.
