Gather's filters, sorts, exposed and contextual filters and facets on an
item field are served by an index on PostgreSQL. At a million items of a 200
field type, the benchmark's field queries went from 0.2 to 16 seconds (two
timed out and two failed) to under 50 ms. Full numbers are in
`docs/performance/field-queries.md`; the design is in
`docs/design/field-indexes.md`.

The cause was that items keep their fields in one JSONB column, and Gather
read a field with `COALESCE(fields->'f'->>'value', fields->>'f')`, an
expression no index covered. The column's only index is a GIN over the
whole document, which none of those conditions can use. So every filter,
sort or facet on a field read every item of the type and decompressed each
item's whole field document (about 5.5 kB, held out of line) to read one
key. A numeric comparison (`fields.price >= 5000`) also failed outright with
`operator does not exist: text >= integer`, because the field was read as
text.

Now:
- Each field has a "Filterable and sortable" setting, on the field form and
  carried by config export and import (`settings.filterable`). A field
  without the setting is on while a gather filters or sorts on it.
- A marked field's value is kept in a small copy column, `item.filter_values`,
  by a trigger on every write. The field gets a B-tree on its value and
  `created`, and PostgreSQL statistics on the expression. A list field
  (tags, several references) is indexed by its elements as a `text[]`, with
  a GIN and element statistics, so a rare tag and a common one each get the
  right plan. A type with marked fields also gets an index on `created` of
  its own items, and a type with three or more marked text fields a GIN over
  its copy, for several equalities at once.
- Indexes are built in the background with `CREATE INDEX CONCURRENTLY`, one
  at a time, after the copy is filled in 256 short statements. Progress shows
  on the content type's fields page. Unmarking or deleting the field drops
  its index. A check at startup and on each cron run rebuilds a missing or
  invalid index and retries a failed build after an hour.
- Gather reads every field through one function, from the copy once the
  field's index is ready. Numbers compare as numbers. Facet option lists walk
  the index, and a facet with a filter applied asks once per option.
- A field that is not marked still works as before, read from the document.
  The Gather editor names it as not indexed.
- Gather's access check reads five columns instead of converting each whole
  row to JSON, which had cost 40% of a simple listing at a million items.
