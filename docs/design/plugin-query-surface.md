# The plugin query surface (B80)

What a plugin uses to do ordinary website work (list content across types,
sorted and paged with each row's URL, build a sitemap, read and write
translations, upsert its own rows, count per group) without `raw_sql`, and what
`raw_sql` becomes. Decision **D-60** below supersedes **D-19**. Parts 2 to 5 of
B80 implement it; a Part that proves a decision wrong amends this note in its
own pull request. Line numbers are at `968d073`.

## 0. Where the tree is

The B80 brief was written at `29c5575`. Re-verified at `968d073`:

| Brief | At `968d073` |
|---|---|
| `raw_sql` is total; the read guard is the first keyword | **Moved.** `raw_sql` still opens both raw calls and skips the allowlist (`plugin/db_policy.rs:323`, `host/db.rs:769,826`), but since #125 (`323e873`) each statement is parsed as a tree (`host/sql_guard.rs:87,96`), may not name a protected table (`db_policy.rs:201`), and `query-raw` runs in `BEGIN READ ONLY` (`host/db.rs:194`). BL-61 was still recorded open; it is fixed. |
| structured `db` calls are minimal | Holds: `do_select` (`host/db.rs:320`), `do_insert` (`:398`, no `ON CONFLICT`), `do_update` (`:443`) and `do_delete` (`:500`) take equality maps only. The SDK binds none of the four; `docs/plugin-development.md:891-937` documents a `host::db::select` with operators that does not exist. |
| `query-items` may leak | **Partly moved.** B68.2 (#163) filters each row through `ItemService::filter_page_for_view` (`host/item.rs:519-526`, `content/item_service.rs:2042`), so access and field access hold. It still calls `Item::list_filtered` (`host/item.rs:500`, `models/item.rs:591`) with **no stage**, ordered by `changed DESC` with no tiebreaker (`models/item.rs:624`). See section 5. |
| Gather is the compiler, no plugin can call it | Holds. `GatherService::execute_definition_with_stages` (`gather/gather_service.rs:704`) validates, injects the viewer, stage and language, over-fetches for access (`:949`) and resolves row URLs in one query (`:897`). The world imports fourteen interfaces now (`crates/wit/kernel.wit:366-380`, `text-format` added by B71); gather is not one, and `tap-gather-extend` is still exported (`:546`). One correction: values are not bound parameters. SeaQuery renders them as escaped literals (`query_builder.rs:187`), which is safe because every identifier is checked first (`gather_service.rs:2037`, `handlers.rs:21`). |
| Gather's language gaps | Holds for joins (`query_builder.rs:460`, one equality per relationship), AND-only filters (`:484`) and no aggregates (BL-30). **Wrong** for JSONB: a sort on `fields.weight` already orders by the JSONB value, not its text (`query_builder.rs:774`, test `:1862`), and `fields.x` projects (`:700`). Statement 3's `weight` needs nothing new. `is_item_table` is `:333`. |
| translations have no kernel write path | **Moved.** `POST /admin/content/{id}/translate/{lang}` and a delete route exist (`routes/admin_translation.rs:31,35`, #94) over `ItemService::save_translation` and `delete_translation` (`item_service.rs:755,810`). Neither checks item access (the route does), fires a tap, or invalidates the item cache; and no host function reaches them (BL-02, "SDK path open"). |

B68 has landed Parts 1 to 3 (`29c5575`, #163, #165): plugin item writes go
through `ItemService` and the SDK binds `item-api`. B80 builds on that: the
translation writes in section 6 are `ItemService` methods called from new
`item-api` functions, decided by the same `acting_for` rule (`host/item.rs:102`).

The plugin request state carries a user and no stage or language
(`tap/request_state.rs:451-479`), and `RequestServices` carries no gather service
(`:225-291`). The seam needs both; Part 2 adds them (section 2).

## 1. The seam

**Decision.** One new host interface, `query`, with one function:

```wit
interface query {
    run: func(definition-json: string, options-json: string) -> result<string, string>;
}
```

`definition-json` is a Gather `QueryDefinition` (`gather/types.rs:15`), the
JSON a site owner's gather is stored as. `options-json` is
`{"page": 1, "per_page": 20, "url_args": {...}}`, every key optional;
`url_args` fills `ContextualValue::UrlArg` (`types.rs:405`). The result is the
`GatherResult` JSON (`types.rs:791`): `items`, `total`, `page`, `per_page`,
`total_pages`, `has_next`, `has_prev`, `access_capped`. The definition is
validated by `GatherService::validate_definition`, compiled by the existing
builder and run by `execute_definition_with_stages`: one query language, not a
second grammar inside `db.select`.

**The ABI.** The `.wit` is documentation (`kernel.wit:339-344`); the contract is
the import module and field names. The kernel registers
`trovato:kernel/query` / `run` with the flattened signature every
string-in-string-out host function has, `(def_ptr, def_len, opt_ptr, opt_len,
out_ptr, out_max) -> i32` (bytes written, or a negative host error), and the
SDK declares the same `#[link(wasm_import_module = "trovato:kernel/query")]`
`#[link_name = "run"]` the other interfaces do (`crates/plugin-sdk/src/host.rs:168,193`).
The whole cost: one entry in `KNOWN_HOST_INTERFACES` (`plugin/info_parser.rs:431`),
one in `HOST_INTERFACE_REGISTRARS` (`host/mod.rs:62`), one host module, an SDK
extern and safe wrapper `trovato_sdk::host::query`, three error codes, and the
WIT text. A plugin opts in with `host_interfaces = ["query"]`; the per-plugin
linker links nothing it did not declare, so no existing plugin changes.
**Additive** under the version map: a new interface.

**Rejected.** Growing `db.select` into joins and operators: a second query
language, with no access, stage or language model. A function on `item-api`: the
seam also serves a plugin's own tables, which are not items. Running registered
gathers only: every listing would need a migration writing `gather_query`
first, and the definition would still be the plugin's.

## 2. The two security models

**Content.** A definition whose base is `item`, or a registered record type
through `record_type`, always runs for the request's viewer, stages and
language:

- the viewer is the request's user (`RequestState.user`), anonymous included;
- the stages are the request's read stages, which Part 2 adds to the request
  state from the one stage resolver (`routes/helpers.rs:921`), Live where the
  dispatch site has none (cron, queue, a tap with no request);
- the language is the request's negotiated language where the dispatch site
  knows it, else none (the base language, no overlay).

The plugin cannot set any of the three. `stage_aware = false` on a content
base is **refused**, not ignored, so a plugin that asks for it learns at once.
A background call (cron, queue) follows `item-api`'s rule exactly: without
`item_background` it is refused with `ERR_ITEM_BACKGROUND_DENIED` (-61); with
it, the viewer is the background principal (`tap/request_state.rs:150`), which
passes `check_access` since B68.2. That is kernel authority under a declared
capability; no path runs a plugin's query as a database superuser or skips the
service.

**A plugin's own table.** A base that is in the plugin's effective allowlist
and is not a kernel content table carries no viewer semantics: `stage_aware` is
forced false, no access or record predicate, no translation overlay, no `url`.
It is what `db.select` already gives that plugin, with Gather's grammar.

**Joins between the two.** Content is always the base. A content base may join
the kernel's join tables (`GATHER_JOIN_TABLES`, `gather_service.rs:2250`) and
the plugin's own allowlisted tables; the access, stage and language predicates
apply to the base rows exactly as without the join. A plugin-table base may join
only the plugin's own tables, never a content table: the per-row access pass
decides base rows (`gather_service.rs:1031-1048`), so `item` on the joined side
would be read with nothing deciding it. Part 2 tests both directions.

## 3. Policy

**Tables.** Every table a definition names that is not a kernel content table
(base and every relationship) must pass `DbPolicy::check_table`
(`db_policy.rs:299`), refused as `table-not-declared` (-16) with the protected
floor first. `GATHER_DENIED_TABLES` (`gather_service.rs:2219`) still applies to
everything.

**Where the brief is wrong.** It asks for every table, content included, to be
in the allowlist. That would make a plugin declare `db_tables = ["item"]` to use
the seam, and that declaration is what opens `db.select` on `item`, which applies
no viewer, stage or access predicate at all. So the seam would require granting
the one thing it exists to replace. Content tables are reached by declaring the
`query` interface, the way `query-items` is reached by declaring `item-api`, and
the kernel's predicates are the authority. Kernel join tables need no
declaration either: a site owner's gather already joins them for anonymous
visitors (`gather_service.rs:2079-2088`), so a plugin gains nothing a visitor
cannot already see.

**Identifiers** are validated twice, as they are for every gather: by
`validate_definition` at the service and by `is_safe_identifier` in the builder.

**`FilterOperator::Custom`** is allowed, whoever registered the name. A custom
filter names a kernel handler (`gather/extension.rs:267-296`); the declaring
plugin supplies a name, not code, and a site owner's gather may use any
registered name already.

**`SemanticSimilarity`** is refused in a plugin definition (`ERR_QUERY_INVALID`,
-80): each execution embeds the query text through the site's provider
(`gather_service.rs:1396`) with no budget principal to charge it to.

**Includes** are refused in a plugin definition. An include is a child gather
with tables of its own; a plugin makes a second `run` call instead, which costs
the same one query.

**Column projection needs no declaration.** Every content row already passes
the field tier: `apply_field_filtering` (`gather_service.rs:1086`) calls
`field_access_decisions` (`item_service.rs:1697`) once per type and drops what
the viewer may not see, for `item.*` and explicit fields alike. For a plugin's
own table the allowlist is the whole grant, as for `db.select`.

## 4. URLs

A content row comes back with `url`, its canonical alias or `/item/{id}`,
from `resolve_row_urls` (`gather_service.rs:897`): one query per page, for the
request's stages and language. A plugin never writes the alias join.

A lookup by alias is a filter on the pseudo-field `url` with `equals` or `in`.
The service resolves each value against `url_alias` for the request's stages
and language in one query and replaces the filter with `id in (...)`; a value
that names no alias matches nothing, and `/item/{uuid}` is accepted as itself.
It lives in `GatherService`, so a site owner's gather can use it too.

## 5. `query-items`

**Finding.** `query-items` still leaks across stages. `Item::list_filtered` has
no stage predicate, and `check_access` lets anyone with `access content` view a
published item on any stage that is not `Internal` (`item_service.rs:1548-1561`).
So a live request lists published items on every `Public` or `Accessible` stage,
and `Accessible` is documented as "not in listings" (`models/stage.rs:33`); a
viewer granted an internal stage sees its drafts in a live request too. The
order is `changed DESC` with no tiebreaker (BL-64).

**Decision.** Deprecated in favour of the seam, and fixed in Part 2: it reads
the request's read stages only and orders by `changed DESC, id DESC`. It is not
reimplemented on the seam, because it returns full SDK `Item` JSON and gather
rows are a different shape; changing an existing function's output is frozen.
The docs point listings at `query::run`.

## 6. Writes

**A plugin's own tables** (`db`, all additive):

- `update` and `delete` accept a `where` that is an **array** of
  `{"field", "op", "value"}` conditions, ANDed, with `op` one of `=`, `!=`,
  `<`, `<=`, `>`, `>=`, `in`, `not_in`, `is_null`, `is_not_null`. That is the
  shape `docs/plugin-development.md:925-935` already documents; the host refused
  an array as `ERR_PARAM_DESERIALIZE` (`host/db.rs:459-460`), so accepting it changes
  no call that worked. A map keeps meaning equality.
- new `upsert(table, data-json, conflict-json) -> result<string, string>`:
  `{"target": [cols], "update": [cols]}`, the update set defaulting to every
  non-target column; returns the row.
- new `insert-many(table, rows-json) -> result<u64, string>`: at most 500 rows,
  the same columns in each, one statement, so it is all or nothing. Over the
  bound it is refused whole (`ERR_DB_BATCH_TOO_LARGE`, -82) before anything is
  written.

All three go through the allowlist and the protected floor as the existing
calls do. The SDK binds `select`, `insert`, `update`, `delete`, `upsert` and
`insert_many` (it binds none of them today).

**Translations** (`item-api`, additive): `list-translations(item-ids-json)`
(at most 100 ids; rows for the items the user may view, with the field tier
applied to `fields`), `save-translation(translation-json)` and
`delete-translation(item-id, language)`. The writes are new `ItemService`
methods that check `edit` access on the item, run the field write gate, fire
`tap_item_update` with the item, and invalidate the item cache; the admin route
moves onto them. Who a call acts as is `acting_for`, so a background write
needs `item_background`. A plugin no longer needs `item_translation` in
`db_tables` at all; it can still declare it, and the capability reference says
why it should not: a `db` write skips access, taps, the cache and the menu
label refresh.

## 7. The language line

A shape that two plausible plugins need belongs in Gather. `raw_sql` keeps what
Gather will not compile: recursive CTEs, window functions, extension-specific
operators (PostGIS, `pg_trgm`, pgvector distance in a plugin's own SQL), locking
reads (`FOR UPDATE SKIP LOCKED`), and migration surgery.

Part 3 adds, inside Gather so a site owner's builder and config get them too:

- **boolean grouping**: `filter_groups`, each `{"conjunction": "and"|"or",
  "filters": [...], "groups": [...]}`, ANDed with `filters`, nested at most 3
  deep;
- **aggregates**: `group_by`, `aggregates` (`count`, `sum`, `min`, `max`,
  `avg`, each with a label), `having`, and `distinct`, **only on a base that is
  not content**, which in practice is a plugin's own table. On `item` they
  are refused: the per-row access pass (`tap_item_access`, the role tier)
  cannot decide an aggregate, so a count would include rows the viewer may not
  see. Record types are refused too (amended in B80.3): their field visibility
  is decided per row after the query, so a `sum` over a hidden field would show
  it. Counting content per group is recorded as a follow-up (BL-30), to be
  computed from the viewer's visible scan rather than in SQL. Columns are plain
  base-table columns; a grouped definition projects its `group_by` columns and
  labels, sorts by them, and its total counts groups.

A grouped member that places no constraint (an empty `in`, an absent URL
argument) is true, as at the top level. Exposed and `semantic_similarity`
filters stay top-level; `url` works in a group. The admin builder has no
control for either shape, so it refuses to save a gather carrying one rather
than drop it.

Rejected: compound join conditions. The case that needed one was the alias join
on source, stage and language, and that is section 4's job now; no second
plugin need was found. JSONB sort and projection already exist (section 0).

## 8. Bounds

For a plugin-submitted definition, each refused with `ERR_QUERY_BOUND` (-81)
rather than silently capped, so a plugin never mistakes a truncated answer for
the whole one:

| Bound | Value | Where it comes from |
|---|---|---|
| rows per call (`per_page`) | 100 | `MAX_ITEMS_PER_PAGE` (`gather_service.rs:2364`); page through the rest |
| relationships | 3 | `validate_definition`'s join cap (`gather_service.rs:2041`), reported as -81 |
| filters, counting every nested group | 32 | new |
| sorts | 8 | new |
| group nesting | 3 | new (Part 3) |
| `group_by` columns; aggregates | 8 each | new (Part 3); `having` counts as filters |
| includes | 0 | section 3 |
| statement timeout | 5 s | the raw path's `PLUGIN_QUERY_TIMEOUT_MS` (`host/db.rs:28`), not gather's 10 s (`gather_service.rs:808`); a cancelled statement is `ERR_SQL_FAILED` (-12), as on the raw path |
| access scan | the site's `GATHER_ACCESS_MAX_SCAN` (1000 by default, `gather/access.rs:73`) | not an error: `access_capped` is true in the result, as for every gather |

A definition the validator refuses for any other reason is `ERR_QUERY_INVALID`
(-80); the host log carries the validator's message. No `EXPLAIN`-based cost
gate.

## 9. What `raw_sql` becomes

It stays, as **one flag**. The read and write gates would be split only if a
read-only `query-raw` were shown to hold against a data-modifying CTE on a real
database while a first-keyword check could not. Part 1 tested it
(`crates/kernel/tests/raw_sql_read_gate_test.rs`): in an ordinary transaction
PostgreSQL runs the `DELETE` in `WITH gone AS (DELETE ...) SELECT ...`, so a
first-keyword check cannot hold; in `BEGIN READ ONLY` the server refuses it with
SQLSTATE `25006` and the row survives. So `query-raw` *is* read-only today, by
the server's decision and not by a keyword. A read-only `raw_sql` would still not
be a narrow grant, though: it reads every table outside the protected floor with
no viewer, stage or language, which is the risk an administrator audits. Splitting
it would add a manifest key and a second thing to audit while removing nothing an
auditor fears. The seam is what narrows it, by making it unnecessary.

**Visibility** (Part 5): the admin plugin listing marks every plugin holding
`raw_sql`; boot logs one line naming them all; `docs/plugin-capabilities.md`
(new) is the capability reference. The sentence the documentation uses:

> `raw_sql` lets a plugin run SQL the kernel does not compile: it can read any
> table outside the protected floor with no viewer, stage or language applied,
> so grant it only to a plugin you trust with all of the site's content, and only
> when the query seam and the structured `db` writes cannot express what it does.

## 10. The six statements, and the next five plugins

trovato.rs (`jeremyandrews/trovato-site`, `plugins/trovato_site/src/lib.rs`)
holds `raw_sql` for six statements. Each, on the seams above:

| Statement | Seam |
|---|---|
| `LISTING_SQL` (`:250`) | `query::run`: base `item`, `type in [blog, news]`, `status = 1`, sort `created desc`, `per_page` = the limit; `url` per row |
| `SITEMAP_SQL` (`:475`) | the same with `type != front_page`, sort `changed desc`, paged by `has_next` |
| `DOCS_SQL` (`:886`) | `type = docs`, fields `title` and `fields.weight`, sort `fields.weight asc nulls last`, `title asc`; rows whose `url` is `/item/{id}` have no alias and are skipped |
| `DOC_MARKDOWN_SQL` (`:779`) | `type = docs`, filter `url = /learn/{slug}`, field `fields.markdown`, `per_page` 1 |
| `TRANSLATION_LIST_SQL`, `_READ_SQL`, `_DELETE_SQL` (`:727-737`) | `item-api` `list-translations` (ids from one `query::run`), `delete-translation` |
| `TRANSLATION_WRITE_SQL` (`:742`) | `item-api` `save-translation` |

Part 5 proves all six with `plugins/test_site_queries`, which declares no
`raw_sql`.

The next five plugins, from the backlog's sources: a blog or news archive
(listing plus count per month: grouping on its own table, or a follow-up for
content), a related-content block (`in` and `not_in` on ids, one call), an
events calendar (date comparisons, `or` groups), a vote or rating plugin
(`upsert` on its own table, `count` per item on its own table), and Netgrasp's
device inventory (its own tables, `insert-many`, comparisons and null tests in
`update`). None of them needs `raw_sql` on this surface. Argus's two atomic
claims (`FOR UPDATE SKIP LOCKED` in a writing CTE, BL-61) stay on `raw_sql`,
which is the line in section 7.

## D-60: plugin query surface (supersedes D-19)

D-19 (WASM-2, Option A) confined structured `db` calls to an effective table
allowlist and let one declared `raw_sql` flag open both raw calls past it. D-60
keeps the allowlist, the protected floor and the flag, and replaces D-19's
premise that raw SQL is how a plugin does anything the structured calls cannot:

1. Plugin reads of content go through `query::run`, compiled by Gather and run
   for the request's viewer, stages and language, which the caller cannot turn
   off (sections 1 and 2).
2. A plugin's own tables are reached through the allowlist, by `query::run` or by
   `db`, whose write grammar gains conditions, upsert and bounded batch insert
   (sections 3 and 6).
3. Translations are written through `ItemService` via `item-api` (section 6).
4. `raw_sql` stays one flag for what Gather will not compile (sections 7 and 9),
   and holding it is shown in the admin listing, the boot log and the capability
   reference.

Every addition is a new interface, function or payload shape that used to be
refused; no existing signature changes.
