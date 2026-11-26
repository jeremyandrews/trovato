# Gather field queries at 200 fields and a million items

How fast Gather answers a filter, sort, exposed or contextual filter or facet
on an item field, on an item type with 200 fields and a million items, measured
through the real Gather service. This page holds the baseline (B207 part 1);
part 2 adds the numbers after the fix beside it.

## Why this was measured

Items keep all their fields in one JSONB column, `item.fields`. The only index
on it is `idx_item_fields`, a default GIN over the whole column
(`crates/kernel/migrations/20260212000005_create_items.sql`), which serves
`fields @> …`, `?`, `?|` and `?&` on the column itself and nothing else. Gather
writes a field condition as an expression on one key:

- `COALESCE(item.fields->'x'->>'value', item.fields->>'x') = 'v'` (and `IN`,
  ranges, `LIKE`), from `GatherQueryBuilder::jsonb_path_sql` through
  `field_value_text_sql`, the one reader of a `{"value": …}` wrapped or bare
  value (`crates/kernel/src/gather/query_builder.rs`);
- `ORDER BY COALESCE(item.fields->'x'->'value', item.fields->'x')`
  (`jsonb_sort_expr`);
- `item.fields->'x' @> json_build_array($1::text)::jsonb` for tags
  (`build_category_filter`, and `JsonbArrayContainsFilterHandler` in
  `handlers.rs`);
- the facet option lists in `gather_service.rs` (`fetch_distinct_values`,
  `fetch_faceted_distinct_values`), with the key bound as `$1`.

No index covers any of these expressions, so every field condition and every
field sort reads every row of the item type that the type, status and stage
indexes leave, and decompresses that row's whole `fields` document to read one
key. The fixture's documents average about 5.5 kB compressed, which PostgreSQL
stores out of line (TOAST): every read of one key fetches and decompresses all
of it, once per reference to the field in the statement.

## The fixture

`benchmarks/field-queries` (`field-bench`) seeds a dedicated database, never
the shared test database; it refuses any database whose name does not start
with `field_bench`. Every value is derived in SQL from a seed and the row
number (`hashint8extended`), so the same options always write the same rows.

- **`bench_main`**, one item type with **200 fields**: 40 short texts
  (cardinality 3 to 1,000,000, uniform and skewed, 0 to 60 percent empty, some
  stored wrapped as `{"value": …}`), 20 long texts, 30 integers (some wrapped),
  20 decimals, 20 booleans, 15 dates, 5 date-times, 15 single references, 10
  multi value references, 15 multi value tag fields (zero to six of 500 tags,
  skewed), and 10 fields that are empty on 95 to 99.5 percent of items. About
  160 keys are present on a typical item.
- **1,000,000 `bench_main` items**, 85 percent published, and **1,000,000
  items** spread evenly over four other types (`bench_place`, `bench_person`,
  `bench_event`, `bench_doc`, 7 fields each), 90 percent published.
- One to three revisions per item (70, 20 and 10 percent): 2,000,000 items,
  2,800,000 revisions.
- 1,000 members, 500 tags, search vectors built by the item trigger as each
  item is written.

Sizes at 1M are in the tables below. **10M was not run**: the 1M fixture
takes 36 GB, so 10M would need about 360 GB against 358 GB free on the Docker
VM's disk. The 100k fixture (same generator) is measured as well, and the 10M
figures are extrapolated from 100k and 1M below.

## The query matrix

24 Gather definitions (`benchmarks/field-queries/src/matrix.rs`), run through
`GatherService::execute_definition` as the anonymous visitor a real request
gets (the anonymous user row with its role's permissions, so the item access
pass runs), ten items a page, and one facet through
`GatherService::fetch_faceted_distinct_values`. A Gather facet is its option
list, the values still reachable under the other active filters, with no
count per value: that list is what Q18 times. Each covers one shape the
brief asks for; the field names say what they hold.

Each query runs 30 times cold and 30 times warm:

- **cold**: before each run, Postgres is restarted (its shared buffers are
  empty) and the Docker VM's Linux page cache is dropped
  (`echo 3 > /proc/sys/vm/drop_caches` in a privileged container). The VM's
  disk image is a file on the Mac, so macOS may still hold some of it in its
  own page cache, which `purge` would need root to drop: cold here is "nothing
  in Postgres or the VM", not "nothing anywhere".
- **warm**: one untimed run, then 30 timed runs back to back. Every timed
  run is attempted, even when the untimed one failed.

Latency is the wall time of the whole Gather call: every statement it sends,
the access pass, URL and author resolution. The statement count is every
`sqlx::query` event the call emits on its own task. The plan is
`EXPLAIN (ANALYZE, BUFFERS)` of each statement that reads `item`, run after
the warm runs; the table shows the slowest one's shape and buffers.

A Gather statement runs with `statement_timeout = 10s`
(`GatherService::run_definition_inner`); a query that hits it fails, and the
tables say so. A failed run counts as slower than every run that finished,
so a percentile whose rank falls on a failure reads "timed out" (or
"failed"), never the time of the runs that happened to finish, and no
speedup is computed from it.

The 1M baseline below was recorded before every timed run was attempted
after a failed untimed one. Its four failing queries say "0 warm" runs
failed because none was attempted. Q03 and Q17 timed out on the untimed run
and on all 30 cold runs; Q04 and Q08 fail at once, on every run, with a
type error.

Item saves are 100 calls of `ItemService::update` on `bench_main` items
(three fields changed, a new revision each), as an administrator. Once
timed, each item row is written back exactly as it was and the revisions
the saves made are deleted, so the next run on the same database queries
the data the seed wrote.

## Hardware and settings

| | |
|---|---|
| Machine | Mac Studio (Mac15,14), Apple M3 Ultra, 32 cores (24 performance, 8 efficiency), 512 GB memory, 4 TB Apple SSD (AP4096Z), macOS 27.2.1 |
| Docker | OrbStack 2.2.5, Docker 29.8.2; the Linux VM has 32 CPUs and 15.7 GiB of memory |
| PostgreSQL | 17.11 (`pgvector/pgvector:pg17`, the image `docker-compose.yml` uses), data on a Docker volume |

PostgreSQL was started with these settings (everything else default):

```
shared_buffers=4GB effective_cache_size=10GB work_mem=32MB maintenance_work_mem=2GB
max_wal_size=32GB checkpoint_timeout=30min random_page_cost=1.1
effective_io_concurrency=200 max_connections=200 max_parallel_workers_per_gather=4
max_parallel_maintenance_workers=8 track_io_timing=on jit=off
```

The 1M fixture is about 36 GB, well over the VM's 15.7 GiB: like a real large
site, most of it is not in memory, which is what the cold runs show.

## Results at 1M (before)

Run `before-1m`, 26 November 2026. Each query 30 times cold and 30 times
warm. Latency is in milliseconds. "Total" is the count Gather reports for the
pager, capped by the access pass's 1000-candidate scan. Buffers and plan are
those of the slowest statement, from `EXPLAIN (ANALYZE, BUFFERS)` after the
warm runs.

| Query | What | Rows | Total | Statements | Warm p50 ms | Warm p95 ms | Warm p99 ms | Cold p50 ms | Cold p95 ms | Buffers (slowest statement) | Plan (slowest statement) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|
| Q01 | equality on one text field (200 values, skewed), newest first | 10 | 1000 | 5 | 467 | 500 | 503 | 721 | 956 | 826060 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q02 | IN on one text field (12 values), newest first | 10 | 1000 | 5 | 210 | 224 | 226 | 238 | 295 | 124319 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q03 | equality on a unique text field (one row) | 0 | - | 0 | timed out | timed out | timed out | - | - | 0 hit / 0 read | ERROR (0 warm, 30 cold runs failed): canceling statement due to statement timeout at line 3406 |
| Q04 | numeric range on an integer field, newest first | 0 | - | 0 | failed: `text >= integer` | failed: `text >= integer` | failed: `text >= integer` | - | - | 0 hit / 0 read | ERROR (0 warm, 1 cold runs failed): text >= integer at line 635 |
| Q05 | date range, sorted by that date descending, page 1 | 10 | 1000 | 6 | 11102 | 11806 | 12500 | 11941 | 13696 | 9600697 hit / 1007260 read | `Gather Merge > Sort > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` |
| Q06 | the same date range and sort, page 500 | 10 | 1000 | 6 | 10963 | 11473 | 12123 | 12097 | 12688 | 9600655 hit / 1007272 read | `Gather Merge > Sort > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` |
| Q07 | every published record sorted by a text field | 10 | 1000 | 5 | 8410 | 8616 | 9270 | 8994 | 9602 | 6307390 hit / 1096755 read | `Gather Merge > Sort > Seq Scan on item` |
| Q08 | three fields ANDed (text, boolean, integer range), newest first | 0 | - | 0 | failed: `text >= integer` | failed: `text >= integer` | failed: `text >= integer` | - | - | 0 hit / 0 read | ERROR (0 warm, 1 cold runs failed): text >= integer at line 635 |
| Q09 | five fields ANDed, newest first | 10 | 449 | 5 | 11130 | 11511 | 11625 | 11550 | 12290 | 9684759 hit / 1006795 read | `Sort > Gather > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` |
| Q10 | three fields ORed, newest first | 10 | 1000 | 5 | 4537 | 4881 | 4932 | 5271 | 5833 | 9085336 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q11 | multi value tag containment (a common tag), newest first | 10 | 1000 | 5 | 231 | 248 | 263 | 279 | 300 | 253643 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q12 | multi value tag containment (a rare tag), newest first | 10 | 1000 | 5 | 744 | 818 | 862 | 1199 | 1532 | 1253432 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q13 | any of three tags, newest first | 10 | 1000 | 5 | 255 | 274 | 297 | 304 | 360 | 441548 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q14 | single reference equality (records at one place), newest first | 10 | 314 | 5 | 6601 | 7175 | 7570 | 7156 | 7735 | 7311978 hit / 1053176 read | `Gather Merge > Index Scan idx_item_created` |
| Q15 | multi value reference containment (records naming one person) | 10 | 1000 | 5 | 4266 | 4614 | 4921 | 4519 | 5200 | 3784097 hit / 1014931 read | `Gather Merge > Index Scan idx_item_created` |
| Q16 | exposed filter answered by the reader, newest first | 10 | 1000 | 5 | 621 | 642 | 648 | 900 | 989 | 1103761 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q17 | contextual filter from a URL argument (records of one owner) | 0 | - | 0 | timed out | timed out | timed out | - | - | 0 hit / 0 read | ERROR (0 warm, 30 cold runs failed): canceling statement due to statement timeout at line 3406 |
| Q18 | facet: the colour options with the country filter applied | 12 | - | 1 | 9843 | 10467 | 11093 | 9722 | 10437 | 20108721 hit / 1007060 read | `Gather Merge > Sort > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` |
| Q19 | a field empty on 99.5 percent of records, newest first | 10 | 72 | 5 | 12023 | 15710 | 16818 | 12702 | 16118 | 1949910 hit / 277237 read | `Index Scan idx_item_created` |
| Q20 | full text search combined with a field filter | 10 | 1000 | 5 | 1313 | 1449 | 1479 | 1829 | 2007 | 1990611 hit / 0 read | `Gather Merge > Sort > Bitmap Heap Scan on item > BitmapAnd > Bitmap Index Scan idx_item_search > Bitmap Index Scan idx_item_type` |
| Q21 | a field nobody marked for indexing (ft_t17), newest first | 10 | 843 | 5 | 6145 | 6879 | 7155 | 7136 | 8437 | 7313568 hit / 1073931 read | `Gather Merge > Index Scan idx_item_created` |
| Q22 | boolean equality (5 percent true), newest first | 10 | 1000 | 5 | 255 | 268 | 274 | 315 | 359 | 437739 hit / 0 read | `Gather Merge > Index Scan idx_item_created` |
| Q23 | every published record sorted by an integer field, descending | 10 | 1000 | 6 | 8568 | 8777 | 8786 | 9129 | 9632 | 6306207 hit / 1097748 read | `Gather Merge > Sort > Seq Scan on item` |
| Q24 | wrapped and bare values: integer equality on fi_rating | 10 | 1000 | 6 | 9437 | 14963 | 15498 | 9410 | 9952 | 6951948 hit / 1099840 read | `Gather Merge > Sort > Seq Scan on item` |

What the table shows:

- **Nothing on a field is served by an index.** The plans read `item` through
  `idx_item_created` (newest first, testing each item's field until the page
  and the 1000-candidate scan are filled), `idx_item_type` (every item of the
  type), or a sequential scan. A filter on a common value is fast only because
  the walk down `created` meets matching items early: Q11 (a tag on a fifth of
  items) takes 231 ms; Q12 (a rare tag) takes 744 ms; Q14, Q15, Q19 and Q21
  (rare values) take 4 to 12 seconds; Q03 (one item among a million) and Q17
  (one owner's items) read past the ten second statement timeout on every run.
- **Every sort on a field sorts the whole type**: Q05 to Q07, Q23 and Q24 take
  8 to 11 seconds, page 500 no slower than page 1 because both sort
  everything.
- **The facet (Q18) reads every published item** of the type to list twelve
  colours: 9.8 seconds.
- **A numeric range on an integer field fails**: Q04 and Q08 end with
  `operator does not exist: text >= integer`. Gather reads the field as text
  and compares it with the number the definition holds.
- **Buffers**: a full read of the type touches about 10 million buffers (80
  GB of buffer reads, mostly TOAST, at about 17 µs of decompression per item).
  "Hit" counts are pages Postgres found in its 4 GB of shared buffers;
  "read" pages came from the kernel, which here is usually the macOS page
  cache, so cold and warm differ by 5 to 50 percent, not by multiples.
- **Statement count**: five or six per Gather call. Two read `item`: the
  access pass's scan of up to 1000 candidates in order, and the window the
  page is cut from; the others are lookups that do not touch fields. The
  facet is one statement.
- **The access scan pays for the whole row.** It turns each of up to 1000
  candidates into JSON (`to_jsonb` of `item.*`) to read five columns, which
  decompresses every candidate's field document: in Q01's plan the scan
  takes 444 ms, of which the index walk is 260 ms and the conversion 184 ms.

### Item saves

100 saves through `ItemService::update` (three fields changed, a new
revision each): **p50 17.2 ms, p99 28.6 ms**.

### Sizes

| Relation | Table | TOAST | Indexes | Total |
|---|---:|---:|---:|---:|
| `item` | 1338 MB | 8352 MB | 16887 MB | 26577 MB |
| `item_revision` | 1192 MB | 8208 MB | 373 MB | 9773 MB |

| Index on `item` | Size |
|---|---:|
| `idx_item_fields` | 16044 MB |
| `idx_item_search` | 449 MB |
| `item_pkey` | 76 MB |
| `idx_item_group` | 75 MB |
| `idx_item_changed` | 57 MB |
| `idx_item_created` | 57 MB |
| `idx_item_language` | 22 MB |
| `idx_item_tenant_id` | 22 MB |
| `idx_item_stage` | 22 MB |
| `idx_item_status` | 22 MB |
| `idx_item_author` | 20 MB |
| `idx_item_type` | 18 MB |

The whole column GIN (`idx_item_fields`) is 16 GB, 60 percent of `item`,
and serves none of the queries above.

### Hot set

After one pass of the matrix from cold, Postgres's 4 GB of shared buffers
held 3,990 MB of `item`'s TOAST table, 103 MB of its TOAST index and 1 MB of
`item` itself: the field queries fill memory with the pages of compressed field documents
they read once and throw away. The buffers were full, so this is a lower
bound, not the working set: pages read early in the pass were evicted by
later ones, and a full read of the type touches about 80 GB of buffers (see
Buffers above). How much memory would keep the matrix resident is not
measured; what it shows is that 4 GB is far short.

## Results at 100k, and what 10M would do

Run `before-100k`: the same generator with 100,000 `bench_main` items and
100,000 of the other types (2.7 GB for `item`), each query 30 times warm and
5 times cold. Q04 and Q08 fail as at 1M. The warm p95s beside 1M, in ms:

| Query | 100k warm p95 | 1M warm p95 | 10M, extrapolated |
|---|---:|---:|---|
| Q01 equality, common value | 843 | 500 | about 500 ms: the walk stops after 1000 matches |
| Q02 IN, common values | 294 | 224 | about 250 ms, same reason |
| Q03 unique value | 2224 | timeout | fails at the 10 s timeout (about 10× 1M) |
| Q05 date range, sorted | 1432 | 11806 | fails at the 10 s timeout (about 10× 1M) |
| Q06 page 500 | 1427 | 11473 | fails at the 10 s timeout (about 10× 1M) |
| Q07 sort on text | 1129 | 8616 | fails at the 10 s timeout (about 10× 1M) |
| Q09 five fields ANDed | 1681 | 11511 | fails at the 10 s timeout (about 10× 1M) |
| Q10 three fields ORed | 2114 | 4881 | 5 to 10 s: rarer matches mean a longer walk |
| Q11 common tag | 479 | 248 | about 250 ms |
| Q12 rare tag | 410 | 818 | several seconds |
| Q14 single reference | 871 | 7175 | fails at the 10 s timeout (about 10× 1M) |
| Q15 multi value reference | 395 | 4614 | fails at the 10 s timeout (about 10× 1M) |
| Q17 contextual filter | 2434 | timeout | fails at the 10 s timeout (about 10× 1M) |
| Q18 facet | 1323 | 10467 | fails at the 10 s timeout (about 10× 1M) |
| Q19 sparse field | 2260 | 15710 | fails at the 10 s timeout (about 10× 1M) |
| Q21 unmarked field | 892 | 6879 | fails at the 10 s timeout (about 10× 1M) |
| Q23 sort on integer | 1115 | 8777 | fails at the 10 s timeout (about 10× 1M) |
| Q24 integer equality, sorted by date | 1153 | 14963 | fails at the 10 s timeout (about 10× 1M) |

Every query that has to read the whole type (a sort, a rare value, the
facet) grows with the type, about eight times from 100k to 1M: at 10M items
it would take 80 to 150 seconds and fail at Gather's ten second timeout. A
filter on a common value stays near its 1M time, because Gather walks the
items newest first and stops after the 1000 candidates its access pass
scans; at 100k the planner chose to read the whole type for Q01 instead, so
it is slower there than at 1M.

Item saves at 100k: p50 14.1 ms, p99 22.6 ms (1M: 17.2 ms and 28.6 ms).

## Results at 1M (after B207 part 2)

Same fixture, same 24 definitions, through the same `GatherService` calls:
`field-bench index` marked the 18 matrix fields and built their indexes,
then `field-bench run`. Q11, Q12, Q13, Q15 and Q18 were measured again
after the last change to list fields (one overlap for "any of these tags")
and merged into the run; the others come from the run before it, on the
same code for them. The Studio was shared with other builds during the
runs (load average 9 to 18).

The before run was made before the benchmark wrote timed saves back, so its
100 saves were still in the data when the after runs read it: the first 100
`bench_main` items by id had `ft_color`, `fi_price` and `fdt_published`
changed and one more revision each. That is 100 of the 1,000,000 items the
queries cover, too few to move a latency, but a query that counts those
values (Q04's price range, Q05's date range) answers over slightly
different rows than the before run did. Both runs are of the same fixture,
which is what `report` checks.

### Before and after

| Query | What | Before warm p50 / p95 / p99 ms | After warm p50 / p95 / p99 ms | Speedup (p95) | Before cold p50 / p95 | After cold p50 / p95 | Before plan | After plan |
|---|---|---|---|---:|---|---|---|---|
| Q01 | equality on one text field (200 values, skewed), newest first | 467 / 500 / 503 | 7.11 / 9.59 / 10.0 | 52.1x | 721 / 956 | 28.3 / 37.1 | `Gather Merge > Index Scan idx_item_created` | `Index Scan item_fx_bench_main__ft_country` |
| Q02 | IN on one text field (12 values), newest first | 210 / 224 / 226 | 9.17 / 11.2 / 11.7 | 20.0x | 238 / 295 | 52.8 / 67.0 | `Gather Merge > Index Scan idx_item_created` | `Index Scan item_fc_bench_main__created` |
| Q03 | equality on a unique text field (one row) | timed out | 1.85 / 2.72 / 3.02 | was failing | - / - | 10.9 / 13.8 | `` | `Index Scan item_fx_bench_main__ft_sku` |
| Q04 | numeric range on an integer field, newest first | failed: `text >= integer` | 11.9 / 13.3 / 13.4 | was failing | - / - | 40.0 / 49.5 | `` | `Sort > Index Scan item_fx_bench_main__fi_price` |
| Q05 | date range, sorted by that date descending, page 1 | 11102 / 11806 / 12500 | 7.68 / 10.4 / 10.9 | 1137.5x | 11941 / 13696 | 28.8 / 36.1 | `Gather Merge > Sort > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` | `Index Scan item_fx_bench_main__fdt_published` |
| Q06 | the same date range and sort, page 500 | 10963 / 11473 / 12123 | 12.7 / 17.5 / 18.8 | 654.4x | 12097 / 12688 | 65.9 / 86.2 | `Gather Merge > Sort > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` | `Index Scan item_fx_bench_main__fdt_published` |
| Q07 | every published record sorted by a text field | 8410 / 8616 / 9270 | 7.77 / 10.9 / 11.5 | 789.1x | 8994 / 9602 | 28.3 / 33.4 | `Gather Merge > Sort > Seq Scan on item` | `Index Scan item_fx_bench_main__ft_name` |
| Q08 | three fields ANDed (text, boolean, integer range), newest first | failed: `text >= integer` | 10.4 / 15.1 / 15.3 | was failing | - / - | 65.5 / 87.1 | `` | `Index Scan item_fx_bench_main__ft_color` |
| Q09 | five fields ANDed, newest first | 11130 / 11511 / 11625 | 33.6 / 36.7 / 36.8 | 313.7x | 11550 / 12290 | 69.4 / 84.3 | `Sort > Gather > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` | `Sort > Bitmap Heap Scan on item > Bitmap Index Scan item_fv_bench_main__all` |
| Q10 | three fields ORed, newest first | 4537 / 4881 / 4932 | 14.5 / 17.3 / 17.8 | 282.9x | 5271 / 5833 | 52.4 / 68.7 | `Gather Merge > Index Scan idx_item_created` | `Sort > Bitmap Heap Scan on item > BitmapOr > Bitmap Index Scan item_fx_bench_main__ft_country > Bitmap Index Scan item_fx_bench_main__ft_t07 > Bitmap Index Scan item_fx_bench_main__fx_recall` |
| Q11 | multi value tag containment (a common tag), newest first | 231 / 248 / 263 | 34.2 / 37.6 / 40.1 | 6.6x | 279 / 300 | 90.7 / 117 | `Gather Merge > Index Scan idx_item_created` | `Gather Merge > Index Scan item_fc_bench_main__created` |
| Q12 | multi value tag containment (a rare tag), newest first | 744 / 818 / 862 | 28.5 / 34.4 / 38.1 | 23.8x | 1199 / 1532 | 111 / 140 | `Gather Merge > Index Scan idx_item_created` | `Gather Merge > Index Scan item_fc_bench_main__created` |
| Q13 | any of three tags, newest first | 255 / 274 / 297 | 31.9 / 35.7 / 36.4 | 7.7x | 304 / 360 | 83.3 / 92.0 | `Gather Merge > Index Scan idx_item_created` | `Gather Merge > Index Scan item_fc_bench_main__created` |
| Q14 | single reference equality (records at one place), newest first | 6601 / 7175 / 7570 | 5.44 / 7.72 / 7.78 | 929.6x | 7156 / 7735 | 19.7 / 25.2 | `Gather Merge > Index Scan idx_item_created` | `Index Scan item_fx_bench_main__fr_place` |
| Q15 | multi value reference containment (records naming one person) | 4266 / 4614 / 4921 | 9.83 / 11.6 / 11.7 | 399.1x | 4519 / 5200 | 30.2 / 37.6 | `Gather Merge > Index Scan idx_item_created` | `Sort > Bitmap Heap Scan on item > Bitmap Index Scan item_fx_bench_main__fm_people` |
| Q16 | exposed filter answered by the reader, newest first | 621 / 642 / 648 | 7.42 / 10.8 / 12.4 | 59.2x | 900 / 989 | 29.7 / 34.9 | `Gather Merge > Index Scan idx_item_created` | `Index Scan item_fx_bench_main__ft_country` |
| Q17 | contextual filter from a URL argument (records of one owner) | timed out | 2.13 / 3.09 / 3.58 | was failing | - / - | 12.0 / 14.8 | `` | `Index Scan item_fx_bench_main__fr_owner` |
| Q18 | facet: the colour options with the country filter applied | 9843 / 10467 / 11093 | 1.33 / 1.55 / 1.80 | 6753.6x | 9722 / 10437 | 12.3 / 16.5 | `Gather Merge > Sort > Bitmap Heap Scan on item > Bitmap Index Scan idx_item_type` | `generic plan: Function Scan on unnest o > Index Scan using item_fx_bench_main__ft_country on item` |
| Q19 | a field empty on 99.5 percent of records, newest first | 12023 / 15710 / 16818 | 3.93 / 5.58 / 6.22 | 2817.8x | 12702 / 16118 | 15.5 / 18.1 | `Index Scan idx_item_created` | `Index Scan item_fx_bench_main__fx_recall` |
| Q20 | full text search combined with a field filter | 1313 / 1449 / 1479 | 18.4 / 20.8 / 21.1 | 69.7x | 1829 / 2007 | 97.8 / 119 | `Gather Merge > Sort > Bitmap Heap Scan on item > BitmapAnd > Bitmap Index Scan idx_item_search > Bitmap Index Scan idx_item_type` | `Index Scan item_fx_bench_main__ft_color` |
| Q21 | a field nobody marked for indexing (ft_t17), newest first | 6145 / 6879 / 7155 | 6131 / 7993 / 8292 | 0.9x | 7136 / 8437 | 7016 / 8716 | `Gather Merge > Index Scan idx_item_created` | `Gather Merge > Index Scan item_fc_bench_main__created` |
| Q22 | boolean equality (5 percent true), newest first | 255 / 268 / 274 | 7.74 / 10.2 / 12.9 | 26.4x | 315 / 359 | 32.4 / 40.8 | `Gather Merge > Index Scan idx_item_created` | `Index Scan item_fx_bench_main__fb_featured` |
| Q23 | every published record sorted by an integer field, descending | 8568 / 8777 / 8786 | 9.14 / 10.9 / 11.4 | 807.3x | 9129 / 9632 | 32.1 / 40.2 | `Gather Merge > Sort > Seq Scan on item` | `Index Scan item_fx_bench_main__fi_price` |
| Q24 | wrapped and bare values: integer equality on fi_rating | 9437 / 14963 / 15498 | 12.7 / 15.9 / 19.0 | 941.5x | 9410 / 9952 | 78.1 / 113 | `Gather Merge > Sort > Seq Scan on item` | `Index Scan item_fx_bench_main__fdt_published` |

Item save p50 / p99: before 17.2 / 28.6 ms, after 13.2 / 20.9 ms.

### After: sizes

| Relation | Table | TOAST | Indexes | Total |
|---|---:|---:|---:|---:|
| `item` | 2179 MB | 8353 MB | 18136 MB | 28668 MB |
| `item_revision` | 1192 MB | 8209 MB | 373 MB | 9774 MB |

| Index on `item` | Size |
|---|---:|
| `idx_item_fields` | 16044 MB |
| `idx_item_search` | 687 MB |
| `item_fv_bench_main__all` | 221 MB |
| `item_pkey` | 98 MB |
| `idx_item_group` | 97 MB |
| `idx_item_created` | 75 MB |
| `idx_item_changed` | 75 MB |
| `item_fx_bench_main__fr_owner` | 65 MB |
| `item_fx_bench_main__fr_place` | 63 MB |
| `item_fx_bench_main__ft_name` | 47 MB |
| `item_fx_bench_main__fm_people` | 43 MB |
| `item_fx_bench_main__fdt_published` | 39 MB |
| `item_fx_bench_main__ft_sku` | 39 MB |
| `item_fx_bench_main__ft_status` | 31 MB |
| `item_fx_bench_main__fi_price` | 30 MB |
| `item_fx_bench_main__fb_featured` | 30 MB |
| `item_fx_bench_main__ft_color` | 30 MB |
| `item_fx_bench_main__fx_recall` | 30 MB |
| `item_fx_bench_main__fi_rating` | 30 MB |
| `item_fx_bench_main__ft_t07` | 30 MB |
| `item_fx_bench_main__fb_b01` | 30 MB |
| `item_fx_bench_main__ft_country` | 30 MB |
| `item_fx_bench_main__ft_t01` | 30 MB |
| `item_fx_bench_main__fi_year` | 30 MB |
| `idx_item_type` | 29 MB |
| `idx_item_status` | 29 MB |
| `idx_item_author` | 29 MB |
| `idx_item_tenant_id` | 29 MB |
| `idx_item_stage` | 29 MB |
| `idx_item_language` | 29 MB |
| `item_fc_bench_main__created` | 21 MB |
| `item_fx_bench_main__fg_topics` | 12 MB |

| Field index (B207) | Kind | State | Build time | Size |
|---|---|---|---:|---:|
| `*` | type | ready | 18.9 s | 221 MB |
| `+` | created | ready | 1.8 s | 21 MB |
| `fb_b01` | text | ready | 1.7 s | 30 MB |
| `fb_featured` | text | ready | 4.2 s | 30 MB |
| `fdt_published` | text | ready | 1.9 s | 39 MB |
| `fg_topics` | list | ready | 5.1 s | 12 MB |
| `fi_price` | number | ready | 2.5 s | 30 MB |
| `fi_rating` | number | ready | 3.1 s | 30 MB |
| `fi_year` | number | ready | 2.4 s | 30 MB |
| `fm_people` | list | ready | 11.8 s | 43 MB |
| `fr_owner` | text | ready | 1.9 s | 65 MB |
| `fr_place` | text | ready | 1.9 s | 63 MB |
| `ft_color` | text | ready | 1.8 s | 30 MB |
| `ft_country` | text | ready | 2.8 s | 30 MB |
| `ft_name` | text | ready | 2.6 s | 47 MB |
| `ft_sku` | text | ready | 2.3 s | 39 MB |
| `ft_status` | text | ready | 3.0 s | 31 MB |
| `ft_t01` | text | ready | 1391.9 s | 30 MB |
| `ft_t07` | text | ready | 2.0 s | 30 MB |
| `fx_recall` | text | ready | 3.0 s | 30 MB |

Hot set (shared buffers holding each relation after one pass of the matrix from cold (shared_buffers caps it)):

| Relation | In shared buffers |
|---|---:|
| `pg_toast_395464` | 3147 MB |
| `item` | 846 MB |
| `pg_toast_395464_index` | 92 MB |
| `item_fc_bench_main__created` | 9 MB |
| `item_fx_bench_main__fdt_published` | 0 MB |
| `users` | 0 MB |
| `pg_proc` | 0 MB |
| `item_fx_bench_main__fb_featured` | 0 MB |
| `item_fx_bench_main__fi_price` | 0 MB |
| `pg_amop` | 0 MB |
| `pg_class` | 0 MB |
| `users_pkey` | 0 MB |
| `pg_proc_oid_index` | 0 MB |
| `pg_attribute` | 0 MB |
| `pg_amop_opr_fam_index` | 0 MB |
| `pg_opclass` | 0 MB |
| `pg_statistic` | 0 MB |
| `pg_operator_oprname_l_r_n_index` | 0 MB |
| `pg_type_typname_nsp_index` | 0 MB |
| `pg_operator` | 0 MB |
| total | 4096 MB |


### EXPLAIN before and after

Five fields ANDed (Q09), the access window:

```
before: Sort > Gather > Parallel Bitmap Heap Scan on item (Filter, 199,910 rows removed per worker)
          > Bitmap Index Scan on idx_item_type (1,000,000 rows)      Execution Time: 5508.517 ms
after:  Sort > Bitmap Heap Scan on item (2,312 rows removed)
          > Bitmap Index Scan on item_fv_bench_main__all
            Index Cond: (filter_values @> jsonb_build_object('ft_status', 'active', 'ft_country', 'c2', 'fb_b01', 'true', 'ft_t01', 'v2', ...))
                                                                       Execution Time: 14.683 ms
```

A date range sorted by the date, page 500 (Q06), the page statement:

```
before: Gather Merge > Sort (Sort Key: COALESCE(fields->'fdt_published'->'value', ...) DESC)
          > Parallel Bitmap Heap Scan on item > Bitmap Index Scan on idx_item_type (1,000,000 rows)
                                                                       Execution Time: 5375 ms
after:  Limit > Index Scan Backward using item_fx_bench_main__fdt_published (5,010 rows read for offset 4,990)
                                                                       Execution Time: 8.303 ms
```

Page 500 costs 8 ms by offset, so Gather keeps offset pagination; keyset
pagination is not needed.

A facet with a filter applied (Q18: the colour options where country = c3):

```
before: SELECT DISTINCT COALESCE(fields->'ft_color'...) ... WHERE COALESCE(fields->'ft_country'...) = 'c3'
        Unique > Gather Merge > Unique > Sort > Parallel Bitmap Heap Scan on item   Execution Time: 9510 ms
after:  option list, a walk down item_fx_bench_main__ft_color, one probe per value (13 loops)
                                                                       Execution Time: 0.184 ms
        then one EXISTS per option: Nested Loop Semi Join > Function Scan on unnest
          > Index Scan using item_fx_bench_main__ft_color (12 loops, 1 row each)
                                                                       Execution Time: 0.670 ms
```

## Reproduce

```sh
docker run -d --name b207-pg -p 127.0.0.1:55207:5432 \
  -e POSTGRES_USER=trovato -e POSTGRES_PASSWORD=trovato -e POSTGRES_DB=b207_bench \
  --shm-size=4g pgvector/pgvector:pg17 postgres \
  -c shared_buffers=4GB -c effective_cache_size=10GB -c work_mem=32MB \
  -c maintenance_work_mem=2GB -c max_wal_size=32GB -c checkpoint_timeout=30min \
  -c random_page_cost=1.1 -c effective_io_concurrency=200 -c max_connections=200 \
  -c max_parallel_workers_per_gather=4 -c max_parallel_maintenance_workers=8 \
  -c track_io_timing=on -c jit=off
docker run -d --name b207-redis -p 127.0.0.1:56207:6379 redis:7-alpine

export DATABASE_URL=postgres://trovato:trovato@127.0.0.1:55207/field_bench
export REDIS_URL=redis://127.0.0.1:56207
cargo build --release -p trovato-field-bench
target/release/field-bench seed --main-items 1000000 --other-items 1000000
target/release/field-bench run --label before-1m --out before-1m.json \
  --explain-dir explain-before-1m \
  --cold-command 'docker restart b207-pg && docker run --rm --privileged alpine sh -c "sync; echo 3 > /proc/sys/vm/drop_caches"'
target/release/field-bench report before-1m.json
# On a branch with the field indexes: mark the matrix's fields, build their
# indexes, run again on the same database, and compare.
target/release/field-bench index
target/release/field-bench run --label after-1m --out after-1m.json \
  --explain-dir explain-after-1m \
  --cold-command 'docker restart b207-pg && docker run --rm --privileged alpine sh -c "sync; echo 3 > /proc/sys/vm/drop_caches"'
target/release/field-bench report before-1m.json after-1m.json
target/release/field-bench drop
```

The before run's timed saves are written back once measured, so the after
run queries the data the seed wrote; `report` refuses a pair of runs of
different fixtures.

Run it from the repository root: the Gather service loads the kernel's templates.
For 100k, point `DATABASE_URL` at `field_bench_100k`, seed with
`--main-items 100000 --other-items 100000`, and run with `--label before-100k
--cold-iterations 5`.
