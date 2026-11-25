The kernel now builds the Pagefind search index itself, and no `pagefind`
binary is needed anywhere: not on a server, not in the image (the Dockerfile no
longer downloads one), not on a developer's machine. This is part 2 of the
incremental index in `docs/design/incremental-search-index.md`. The new
workspace crate `trovato-search-index` writes Pagefind 1.3.0's format (CBOR
through minicbor, gzip with Pagefind's signature, SHA-1 file names) and carries
Pagefind 1.3.0's own tokenizer, the `fossick` module, vendored under its MIT
licence, together with the 1.3.0 browser bundle. It does not depend on the
`pagefind` crate, so neither `pagefind` nor `actix-web` enters the root
lockfile. On the largest local content set (1,237 items) a rebuild takes
0.31 to 0.36 seconds with every page tokenized and 0.21 to 0.23 seconds when
nothing changed, where the CLI took 1.6 to 2.5 seconds.

Every searchable item now has a page number for life in a new ledger table,
`search_page`; a deleted item's number becomes a tombstone that the next new
item in its language takes. Each item's tokenized terms are cached in
`search_page_terms`, keyed by a hash of the HTML it was indexed from, so a
rebuild re-tokenizes only what changed. `search_index_state` records the
published generation and the Pagefind version, and `search_index_file` when
each index file stopped being used. A rebuild reads the published generation before it
exports the content, holds a Postgres advisory lock, stages the new index beside
`PAGEFIND_OUTPUT_DIR`, and publishes it only if no other build published since
that read: data files first, then the meta file, then `pagefind-entry.json`
last, by rename. A build that lost to another one changes nothing and asks for
another rebuild, so an older export never replaces a newer one; so does a
build whose entry went live but whose database transaction then failed. File
names are seven hash characters, so the build names its files against the live
directory, and a name already used there by a different file is lengthened
rather than reused. The old index is no longer moved
aside: a browser that loaded it keeps finding its files, which are deleted 24
hours after the published index stops using them. A build that wrote no entry
file, or an entry naming a missing meta file, is refused before the published
index is touched; before, a CLI that exited cleanly without writing an index
left the site with none. The `rebuild_requested` flag works as before.

Search results change in one way, on purpose: each item is indexed in its own
language (`<html lang>` from the item's language, or the site's default), so
Pagefind stems words and the browser loads that language's search code. On
the local content set, "commented" now finds 83 pages where it found 7, and
"lines", "titled", "updated" and "revisioned" likewise find their other forms.
A query that is the start of a longer token and ends in a letter the stemmer
changes can find fewer: "zqbody" stems to "zqbodi", which no longer begins
`zqbody01…`. Items in different languages are now in separate indexes, so an
English search no longer returns German pages. A single language index was
called `unknown` before and is now `en` (or the site default).

Pagefind's fixed-name files (`pagefind.js`, `pagefind-entry.json`, the
`wasm.*.pagefind` files) are now served with `Cache-Control: no-cache`; the
content-addressed index files keep their day.

Three compatibility gates hold the format to Pagefind's own, over a 60 item
English and German fixture (`search_index_gates_test`): two builds are byte
identical, the index decodes equal to the pinned CLI's, and the stock
`pagefind.js` in Node returns the same results from both. The Test job in CI
installs the pinned CLI and Node for the last two and fails if either skips.
The pagefind harness now compares the vendored tokenizer and the native build
with the CLI as well, and gains `measure-native` and `bundle` commands.
