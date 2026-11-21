`raw_sql` is visible wherever an administrator looks, and the site work it was
held for no longer needs it.

- The admin plugin listing (`/admin/plugins`) marks every plugin whose
  manifest grants `raw_sql`, and says above the table what the grant means.
- Boot logs one warning naming every loaded plugin that holds `raw_sql`.
- `docs/plugin-capabilities.md` (new) says what each `[capabilities]` key
  grants, and what `raw_sql` costs.
- `plugins/test_site_queries`, a test fixture, does the six statements a real
  site plugin held `raw_sql` for (a listing, a sitemap, a docs index, a doc by
  its alias, and a translation sync that lists, writes and removes) through
  `query` and `item-api`, with no `db`, no `db_tables` and no `raw_sql`.
- The error-handling example in `docs/plugin-error-codes.md` used `query_raw`
  on `item`; it uses `host::query`. `docs/design/Design-Plugin-SDK.md` named a
  `[permissions] requires = ["raw_sql"]` key that never existed; it names
  `[capabilities] raw_sql = true`.

Contract: **additive**. `PluginInfo::holds_raw_sql` and
`PluginRuntime::raw_sql_holders` are new public kernel functions; no WIT,
SDK or manifest shape changed. The boot warning is new log output (see
`UPGRADING.md`).
