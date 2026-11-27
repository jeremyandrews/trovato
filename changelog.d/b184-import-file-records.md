`trovato import` now accepts a `file` record: a file a port has already copied
into the files directory, registered under the id, owner and date the record
gives, with no second upload of the bytes.

The importer reads the file at the path the record names — inside the files
directory only, no `..`, no symlink leading out — checks it the same way an
upload is checked (size, MIME allowlist, magic bytes, the name's extension),
measures its own size rather than trusting the record, and writes one
`file_managed` row pointing at that path. An item can reference an imported
file exactly as it references an uploaded one, because the file reference seam
only ever looks a file up by its id or storage URI. A rerun of an unchanged
file record writes nothing; one whose bytes are missing fails that line with a
clear message, inside the one transaction the rest of the import already uses.
