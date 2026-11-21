The server checks its uploads directory at startup: it writes, reads back and
deletes a uniquely named file in `UPLOADS_DIR`, and refuses to start, naming
the directory, if any step fails. A missing or read-only mount used to surface
only when someone uploaded a file, and on several servers only on the one whose
mount was broken.

`PAGEFIND_OUTPUT_DIR` says where the Pagefind index is built and served from.
It defaults to `pagefind` under the first `STATIC_DIR`, where the index has
always gone, and its URL stays `/static/pagefind/`; files under that URL are
now served from this directory only. On several servers, point it into the
shared volume, so the index one server builds is the one every server serves.
The swap is unchanged in kind: the build is made beside the output directory,
in the same parent, then the old index is renamed aside and the new one
renamed in. The build and backup directories are now named with a fresh id
rather than the process id, which is 1 in every container.
