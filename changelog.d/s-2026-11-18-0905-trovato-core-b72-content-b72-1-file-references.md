**Behavior change.** A file in a File field is now served to every viewer of a
published item that references it, kept on every write path, and uploads up to
the documented 10 MB work. A File field stores the bare `file_managed` id, but
the reference index that `/files/{path}` consults was built only from storage
paths found in field text, so a file attached through a File field had no
`file_reference` row and was served as an orphan, to its uploader and
administrators only. It was also promoted out of `Temporary` only by the two
admin content form routes; the item API, plugin `save-item` and `config import`
left it to the six hour temporary file cleanup, which deleted it.

One seam, `content::file_refs`, now reads an item's references (File field ids
from the type's field definitions, plus the existing `local://` and `/files/`
scan) and, inside the item's own write transaction, replaces its
`file_reference` rows and promotes every referenced temporary file, in three
statements whatever the number of files. `ItemService` create, update and
revision revert, and `config import`, all run it; the admin form's own
promotion is gone. A write that adds a reference to a file the acting user may
not be served (someone else's unattached upload, or a file held only by items
they cannot view) is refused with 403, decided in one query for the whole set;
config import, operator trusted, is exempt. A migration backfills references
for File field values already stored and promotes the files they name.

Item templates get `referenced_files` beside `referenced_items`: for each File
field, `{id, url, filename, mime, alt}` per file the viewer may fetch, resolved
in one query.

Uploads up to 10 MB work: the upload routes had no body limit of their own, so
axum's 2 MB default refused anything larger with a generic 400. They now accept
`MAX_FILE_SIZE` plus multipart framing, and a larger file is a 413 that says
it is too large. Upload refuses a filename whose extension does not map to the
validated type (a PNG named `x.svg`), using one extension table that serving
shares. `/files/{path}` sends the stored, validated `filemime` instead of a
guess from the path, and the guess table, used only for paths with no
`file_managed` row, no longer maps `.svg` to `image/svg+xml`. The unused
`"s3"` arm of the storage scheme match and `LocalFileStorage::generate_uri`,
which built URIs from 8 uuid characters, are removed.
