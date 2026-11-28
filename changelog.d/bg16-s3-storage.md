Uploaded files can live in any S3 compatible bucket (AWS S3, MinIO, or another
provider's S3 API) with `FILE_STORAGE=s3` and `S3_BUCKET`, so any number of
servers share uploads without a shared filesystem. A directory (`UPLOADS_DIR`)
stays the default, and single server installs change nothing.

A file keeps the URI it had (`local://<key>`) in either store, so no row, field
or item body is rewritten, and `trovato files migrate-to-s3` copies an existing
directory into a bucket, reading every copy back to check its SHA-256 and
skipping what is already there, so an interrupted run is finished by running it
again. Files are still served only through `/files/` after the access check;
the bucket is never read by anyone but the servers. Image style derivatives are
read and written through the file store too, so one server makes a derivative
and every server serves it. Each server checks at startup that it can write,
read back and delete an object in the bucket, and the status report does the
same. A file missing from the store is now recognised by its error type rather
than by words in the error text that the directory store never used, and a
write to a directory is a rename of a finished temporary file, so a reader never
sees half a file. The design of the hosted network's production setup on AWS,
which this is the first part of, is `docs/design/production-hosting.md`.
