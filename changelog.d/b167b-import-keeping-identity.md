`trovato import <file>` imports items and their revision history from a
newline delimited JSON file, keeping each record's id and its created and
changed dates, so a site migration no longer writes Trovato's tables with SQL.

`CreateItem` has no id and no date, so `ItemService::create` dates every item
now and gives it a new id; a port that wanted to keep the old site's ids and
dates had to write `item` and `item_revision` directly, in its own
transaction, and then restart the server, because nothing told the running
servers to drop their cached copies. It depended on table layouts that are
not an interface, and skipped every check the kernel makes on a write.

The importer (`crates/kernel/src/import/`) checks each record as an
administrator's form save is checked (the JSON API's field check with every
text format allowed, the required fields, and the page builder, category,
cardinality and file reference checks `ItemService` runs on every write),
writes the whole file in one transaction, and bumps every cache kind once at
commit. It upserts by id: an identical record writes nothing, so a rerun
reports 0 changes, and a changed item gains one revision dated as the record
says. A stored revision is never rewritten. A record that fails names its line
and nothing is written. The power to choose an id or a date exists only in
this command; no input struct, route, API or plugin host function gained it,
and the form create and edit paths are unchanged. The format and the design
are in `docs/design/import.md`, with a worked example in
`docs/migrating-a-site.md`.
