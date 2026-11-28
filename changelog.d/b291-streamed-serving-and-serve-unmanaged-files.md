`/files/` and an image style derivative now stream a file's bytes instead of
holding the whole thing in memory, on a local directory and on an S3 compatible
bucket alike, and both answer a `Range` request: a single `bytes=a-b`, `a-` or
`-n` gets `206` and `Content-Range`, a range past the end gets `416`, and every
response carries `Accept-Ranges: bytes`. Audio and video can now seek. Access is
still decided before a single byte is read, exactly as before.

`trovato import`'s `file` record now reads, measures and validates the file it
registers through the site's own file store rather than the local filesystem
directly, so a `file` record works on a site with `FILE_STORAGE=s3` as well as
one with a local `UPLOADS_DIR`. The input contract is unchanged: a relative
path, no `..`, no absolute path, and on a local directory the same symlink
containment check as before.

A new setting, `SERVE_UNMANAGED_FILES` (default `true`, which keeps every
existing site serving exactly as it does today), lets a ported or hosted site
say `false` instead: `/files/` and a derivative then refuse any stored path with
no `file_managed` row, before storage is ever read, rather than falling back to
serving it to anyone. A site migrated from another CMS copies its files into the
store ahead of the importer, and until every one of them has gone through
`trovato import`'s `file` record, an unregistered file (a Drupal `private://`
file, for instance) would otherwise be world readable through the plain file
route. The status report's file system section shows the setting's value.
