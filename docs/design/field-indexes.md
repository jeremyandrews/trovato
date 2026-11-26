# Indexes for Gather's field queries (B207)

Status: decided and built in B207 part 2.

## The problem

Items keep their fields in one JSONB column, `item.fields`. Its only index is a
GIN over the whole column, and Gather writes every field condition, sort and
facet as an expression on one key of that column, which no index covers. So at
a million items of a type, each field query reads every item of the type and
decompresses each one's whole field document to read one key. A document of a
200 field type is about 5.5 kB compressed, stored out of line (TOAST), and
costs about 17 µs to decompress. The baseline is in
[`docs/performance/field-queries.md`](../performance/field-queries.md): most
field queries take 0.2 to 16 seconds at a million items, several reach the ten
second Gather statement timeout, and a numeric range on an integer field fails
with `operator does not exist: text >= integer`.

The aim: every Gather filter, sort, exposed filter, contextual filter and facet
on an item field is served by an index, at 200 fields and millions of items,
with p95 under 50 ms for every query of the matrix on an indexed field.

## What other systems do

| System | How a filterable field is stored and indexed |
|---|---|
| Drupal 10/11 | One table per field (`node__field_x`: one row per value, delta and language), with an index on the entity id and on each column the field type declares. Views joins one table per filtered or sorted field. The JSON storage work (a `json` type in the Schema and Database API, which landed in 2026, issue #3343634, and the JSON field storage plan, #3369988) is about storing structured values in one column, not indexing them; querying into JSON still depends on each database's support. |
| WordPress | Custom fields are rows in `wp_postmeta` (`post_id`, `meta_key`, `meta_value` LONGTEXT), indexed on `meta_key` and on a 191 character prefix of `meta_value`. Each `meta_query` clause joins the table again, and a numeric comparison casts `meta_value`, which no index serves. |
| Directus | Every field is a column of the collection's table. A field can be marked indexed or unique, which creates an ordinary B-tree. |
| Strapi | Every field is a column. Indexes other than unique ones are not declared in the schema; a site adds them by hand or in a migration. |
| Payload | On PostgreSQL (Drizzle), each field is a column; `index: true` on a field, or a collection's `indexes` list, creates B-trees. Arrays and blocks become child tables, or optionally JSON, which is not indexed. |
| Wagtail | Page fields are model columns. StreamField is JSON (`jsonb` on PostgreSQL) and is neither queried nor indexed: filtering on a value means making it a model field, and `search_fields`' `FilterField` marks what the search backend may filter on. |

Every system that filters large tables quickly does it on a column (Drupal's
per-field tables, Directus, Strapi, Payload, Wagtail); none indexes into a
document. That is what the measurements below found too.

## The designs compared

All on one copy of the 1M fixture (`field_bench`: 1M items of the 200 field
type, 1M of four others), PostgreSQL 17, warm, each statement 30 times. The
statements are the shapes Gather sends: the access pass's scan of up to 1000
candidates in order (the costlier of its two item statements), and the facet
queries. p95 in ms; the matrix's 18 fields indexed, `ft_t17` left unmarked.

- **(a) Expression B-trees on `fields`**: `((COALESCE(fields->'x'->>'value',
  fields->>'x')), created) WHERE type = 'T'`, a GIN for list fields.
- **(b) Generated columns**: `ALTER TABLE item ADD COLUMN g_x … GENERATED
  ALWAYS AS (reader) STORED` and a B-tree on each.
- **(c) Side table**: `item_field_value(item_id, item_type, field, value_text,
  value_num, created)`, one row per value, B-trees on `(type, field, value,
  created)`; conditions as `EXISTS` subqueries.
- **(d) One GIN `jsonb_path_ops` over the whole `fields` column**,
  conditions as containment, with (a) for ranges and sorts.
- **(e) A copy column**: `item.filter_values`, a small JSONB column that
  holds only the marked fields' bare values and stays in the row, kept by a
  trigger; expression B-trees on it as in (a), plus statistics on each
  expression.

| Shape | (a) | (b) | (c) | (d) | (e) | (e) + statistics |
|---|---:|---:|---:|---:|---:|---:|
| Q01 equality, common value | 1.8 | 1.7 | 51.1 | 228.6 | 1.7 | 2.0 |
| Q02 IN of three values | 136.3 | 3.8 | 788.9 | 8831.5 | 271.7 | 5.9 |
| Q03 equality, unique value | 0.7 | 0.6 | 0.6 | 0.5 | 0.7 | 0.6 |
| Q04 integer range | 2.8 | 2.8 | 7.2 | | 3.1 | 3.1 |
| Q05 date range sorted by the date | 23.4 | 1.8 | | | 2.0 | 2.2 |
| Q06 the same, page 500 | 114.4 | 6.6 | 13.7 | | 7.8 | 6.7 |
| Q07 sort on a text field | 23.7 | 1.9 | 3.5 | | 1.9 | 2.2 |
| Q08 three fields ANDed | 723.6 | 7.6 | 646.5 | 2182.7 | 45.6 | 6.5 |
| Q09 five fields ANDed | 13169.8 | 41.8 | 215.7 | 7199.4 | 426.6 | 54.3; **15.4** with the type GIN |
| Q10 three fields ORed | 4.7 | 4.5 | 248.3 | 83.2 | 10.0 | 5.1 |
| Q11 a common tag | 1305.1 | 25.9 | 184.5 | 282.1 | 190.3 | 37.3 |
| Q12 a rare tag | 170.6 | 9.7 | 15.4 | 87.5 | 12.4 | 12.7 |
| Q13 any of three tags | 323.5 | 33.9 | | | 36.0 | 35.3 |
| Q14 single reference | 1.1 | 1.0 | 1.5 | 5.4 | 1.3 | 1.2 |
| Q15 multi value reference | 32.3 | 2.9 | 3.7 | 18.6 | 4.1 | 3.6 |
| Q17 contextual filter | 0.6 | 0.5 | | | 0.9 | 0.6 |
| Q18 facet, one question per option | 13.4 | 1.1 | | | 7.9 | 1.1 |
| Q18 facet, rare scope | 101.6 | 1.8 | | | 6.2 | 2.5 |
| Q18 facet, 200 options | 1563.7 | 2.9 | | | 119.2 | 4.2 |
| Q18 facet as one `DISTINCT` | 1289.0 | 32.2 | 265.6 | 806.6 | 43.1 | 47.7 |
| Option list, walk down the index (200 values) | 5.7 | 1.2 | | | 3.1 | 1.4 |
| Option list as one `DISTINCT` | 40519 | 123.4 | | | 1304.7 | 264.3 |
| Q19 sparse field | 0.7 | 0.8 | 0.8 | 1.9 | 1.4 | 0.7 |
| Q20 full text and a field | 50.0 | 13.7 | | | 68.5 | 15.8 |
| Q22 boolean | 1.6 | 2.3 | | 768.7 | 1.9 | 2.1 |
| Q23 sort on an integer | 23.6 | 2.3 | 10.7 | | 1.8 | 1.8 |
| Q24 integer equality, sorted by a date | 3430 | 6.1 | | | 203.6 | 8.6 |

An empty cell: the design has no better answer than another column's, or the
shape was not run for it. What it costs to set up, at 1M items of the type:

| | (a) | (b) | (c) | (d) | (e) |
|---|---|---|---|---|---|
| Adding fields | 5 to 9 s per index, `CONCURRENTLY` | 964 s for 18 columns, **the table locked** (`ACCESS EXCLUSIVE`: no reads or writes) while it is rewritten | 595 s to fill (19.4M rows, 3.8 GB) | 364 s, `CONCURRENTLY`, 2.3 GB | 769 s to fill, in 256 statements of at most 6.4 s holding row locks only; then 5 to 9 s per index, `CONCURRENTLY` |
| Each save | the item's indexes | the same | every value row of the item rewritten | the whole document re-indexed | the trigger recomputes the copy (a few µs), the item's indexes |
| Adding a type's 201st marked field | an index | a column of `item`, for every type (1,600 columns at most, dropped ones included) | rows | nothing | an index |

### What the numbers say

- **Reading one key of the document is the cost.** (a) indexes the right
  expressions, and a condition on one indexed field is as fast as anywhere
  (Q03, Q14, Q17, Q19). But a second condition, a sort by another field or a
  facet must read the field from each row the index returns, which means
  decompressing that row's 5.5 kB document: Q08, Q09, Q11 and Q24 take 0.7
  to 13 seconds.
- **(b) is fast because the values are in the row**: checking another
  condition costs nothing. It passes every shape. But adding a generated
  column rewrites the whole table under an exclusive lock: sixteen minutes
  in which the site can neither read nor write an item, every time a field
  is marked. And every column is a column of every type.
- **(c) and (d) do not pass**: a join per condition, or a bitmap of every
  matching row sorted afterwards, is slower than a walk down the right index
  as soon as a value is common (Q02, Q08, Q22).
- **(e) puts the values in the row without a rewrite**: adding a nullable
  column is instant, and a background job fills it in short statements while
  the site runs. Its first measurement (column "(e)") still lost to (b) on
  Q02, Q09, Q11 and Q24, by choosing plans a hundred times slower. The cause:
  PostgreSQL estimates how many rows match an expression from statistics on
  that expression, which `ANALYZE` gathers for an expression index, but the
  planner ignores those of a partial index, and every field index is partial
  on its item type. With `CREATE STATISTICS` on each expression (PostgreSQL
  14 and later), the planner estimates as well as for a column, and (e)
  matches (b): every shape under 50 ms except five equalities ANDed (54 ms).
- **Several equalities at once**: the five field AND (Q09) has 38,000 items
  in its most selective field and 449 that match all five; the B-tree on that
  field still visits 38,000 rows. A GIN over the whole (small) copy column
  intersects the fields' lists of items instead: 15 ms. But the planner
  underrates containment, and given it for two equalities (Q08) it chose the
  GIN where the B-tree walk was twenty times faster (132 ms against 6.5). So
  Gather adds the containment only for three or more equalities on served
  text fields.

## Decision

**(e): a copy column with an index and statistics per marked field.**

- `item.filter_values JSONB` holds, for each item, the bare values of its
  type's marked fields: text as a JSON string, a number as a JSON number
  (`trovato_field_numeric`), a list as its array. It is small (a few hundred
  bytes), so it stays in the row, uncompressed.
- The trigger `item_filter_values` (`BEFORE INSERT OR UPDATE OF type,
  fields`) recomputes it from `fields` on every write, whoever writes. The
  kernel rewrites the trigger's function whenever the marked fields change
  (`field_sql::trigger_function_sql`).
- Per marked field: a B-tree on `((reader), created) WHERE type = 'T'`, and
  `CREATE STATISTICS` on the reader. A list field (tags, several references)
  is indexed by its elements as a `text[]` (`trovato_field_list`), with a
  GIN and statistics on that. The statistics then hold the most common
  elements, which PostgreSQL uses to estimate `@>` on an array; for a
  `jsonb` array it has only whole-array statistics. With a jsonb GIN, a rare
  tag at a million items was found by walking every item newest first
  (1.4 s) rather than through the index (8 ms).
  Per type with three or more marked text fields: a GIN `jsonb_path_ops` on
  the whole copy. Per type with any marked field: a B-tree on `created
  WHERE type = 'T'`. A tag, an `OR` or a field that is not marked has no
  index ordered by `created`. A listing on one of those, newest first, used
  to walk `idx_item_created`, which holds every type's items. At a million
  items of the type and a million of others, a common tag (5.6% of items)
  visited 63,000 index entries for its 1,000-row access window; now it
  visits 21,000, in 35 ms instead of 58.
- A field is marked by its "Filterable and sortable" setting
  (`settings.filterable`, on the field form and in config export and
  import). A field without the setting is marked while a registered gather
  filters or sorts on it.
- Marking a field queues a job (`item_field_index`), which fills the copy for
  the type's existing items in 256 short statements, creates the statistics,
  builds the index with `CREATE INDEX CONCURRENTLY` and analyzes the table;
  progress shows on the content type's fields page. Unmarking or deleting the
  field drops the index (`DROP INDEX CONCURRENTLY`) and statistics. A check
  at startup and on every cron run requeues a marked field whose index is
  missing or invalid, and a build that failed an hour or more before.
- Builds take turns, on one server or several. One fill per type runs at a
  time, and covers every marked field of the type, so the others skip theirs.
  One statistics, index or `ANALYZE` change runs on `item` at a time:
  `CREATE STATISTICS` and `CREATE INDEX CONCURRENTLY` both take the table's
  SHARE UPDATE EXCLUSIVE lock, which conflicts with itself. Building an
  expression or partial index concurrently also waits for every older
  transaction. Run together, ten of the first nineteen builds at a million
  items failed with `deadlock detected`. The turns are advisory locks, taken
  by asking again every half second (`pg_try_advisory_lock`), not by waiting
  inside a statement. A statement waiting for a lock is itself an older
  transaction, which the build holding the lock would wait for.
- Gather reads every field through one function, `field_sql::read`: from the
  copy once the field's index is ready, from `fields` before that and for any
  field not marked. Both give the same answer. The Gather editor names the
  fields a gather uses that have no ready index.
- Facets ask once per option whether any item in scope has it (each answered
  by an index and stopping at the first match), with the option list from a
  walk down the field's index, one probe per distinct value.

## Consequences

- An unmarked field still works, read from `fields` as before: on a large
  type, a full read of the type (6 s for `ft_t17` at a million items).
- A marked field costs, per million items of its type: about 30 MB of
  B-tree, a few bytes in each row's copy, and a little on each save. The
  benchmark's saves (`ItemService::update`, 18 fields marked) are in the
  performance page.
- The copy can hold a key after its field is unmarked, until the item is
  next saved; nothing reads it.
- `item.*` now includes `filter_values`; Gather removes it from its rows.
- Ties in a sort keep PostgreSQL's order, which can differ between the
  indexed and the unindexed plan (as it could before between any two plans):
  a listing that needs a stable order sorts by a second key.
- The content type and gather registries are caches whose entries expire.
  Gather loads a type's definition before it reads the type's field kinds,
  and the reconcile reads both registries from the database. Before that,
  a gather run after the type's entry expired read every field from
  `fields` as text, and a reconcile then would have dropped every index.
- A server learns that another server built or dropped an index on its next
  reconcile (a field or gather save, a cron run); until then it keeps reading
  as it did. A field unmarked on another server is read from its stale copy
  for up to one cron interval.
