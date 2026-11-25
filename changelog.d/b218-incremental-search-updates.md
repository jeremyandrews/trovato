The Pagefind search index is now updated item by item. An edit or a new comment
is searchable at the next cron run, after an update that rewrites only that
item's part of the index. Before this change, the whole index was rebuilt.

The cause was that every change asked for a full rebuild. The item service, the
comment service and the content type screen each set
`pagefind_index_status.rebuild_requested`, and `trovato_search`'s `tap_cron`
set it too whenever `MAX(changed)` over published items moved. One comment
therefore cost an export, render and encode of every searchable page. This is
part 3 of `docs/design/incremental-search-index.md`.

On the largest local content set (1,237 searchable pages) the numbers are:

* a title edit is searchable 40 to 45 ms after the write starts, and a comment
  34 to 40 ms after;
* the cron update takes 20 to 22 ms of that and writes 5 files (the item's
  fragment, the two word chunks its words are in, the meta file and
  `pagefind-entry.json`);
* the same edit followed by a full build took 260 to 277 ms and re-encoded every
  page.

Writers now queue the items they changed in a new table, `search_index_queue`.
It holds one row per item, so the last write before cron wins, and records a
reason. The writers are:

* the item service;
* the comment service;
* the content type search switches, the search field weights and the reindex
  button, each of which queues every item of the type;
* workspace publishing, inside its own transaction, for every item it moves or
  deletes;
* URL alias writes, because the alias is the result's link.

Plugins have one new host function, `search-index-enqueue` in the `db`
interface (`host::search_index_enqueue` in the SDK). The rule for plugin
authors, in `docs/plugin-development.md`, is this: a plugin that writes an
item, its comments or its URL alias without the item API calls it afterwards
with the ids it changed. Writes through `save_item` and the kernel services
are queued already.

Scheduled publishing used to change `item.status` with raw SQL, which nothing
saw, so a scheduled unpublish left the item in search results until something
else rebuilt the index. It now writes each change through the item API, so the
item is queued, a revision is recorded and the write taps fire. A scheduled
unpublish therefore takes the item out of the results at the next cron run.
`trovato_search` no longer polls `MAX(changed)`. Its `tap_cron` reports how
many items are queued.

How a cron run applies the queue:

* It drains the queue in batches of 200 items.
* For each batch it exports only those items, through the same access seam as
  a full build, and tokenizes only pages whose HTML changed.
* It rewrites the word chunks their old and new words are in. The chunk ranges
  are recorded in a new table, `search_chunk`, and stay frozen until the next
  full build. A chunk past 40,000 locations splits, and one that empties is
  dropped.
* It publishes a new meta file and then the entry file. That happens under the
  same advisory lock and generation check as the full build.

The full build remains, and is taken with a `warn` log line naming the reason
when an update's preconditions fail. Examples are a live entry the database
did not record, a missing cached term set, a new or emptied language, or a
backlog of more than 1,000 items that is more than a quarter of the index. An
operator can ask for one with the new `trovato search rebuild` command.

Deleted and unpublished items leave tombstones in their language's page slots,
and new items reuse them. When tombstones pass a share of a language's slots,
cron compacts the index with one full build that renumbers densely. The result
is byte for byte the index a first build of the same content writes. The share
is a new search setting, "Compact the search index above this percentage of
freed slots", at `/admin/config/search`. It defaults to 5% and takes 0.5 to 50.

A browser tab that loaded the index more than 24 hours before a search can ask
for a chunk or fragment that has since been deleted. `scolta.js` now reloads
the index once when a search fails that way, and runs the search again.
