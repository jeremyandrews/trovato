A plugin can do the ordinary writes on its own tables without `raw_sql`, and
it can write translations through the item service.

On the plugin's own tables (`db`):

- `update` and `delete` take a `where` that is an array of
  `{"field", "op", "value"}` conditions, ANDed. `op` is one of `=`, `!=`, `<`,
  `<=`, `>`, `>=`, `in`, `not_in`, `is_null` or `is_not_null`. The plugin
  guide documented this shape, and the host refused it with
  `ERR_PARAM_DESERIALIZE`. A map still means equality. An empty `where`, an
  unknown op or an empty `in` list is refused, so none of them can mean every
  row.
- `upsert(table, data, conflict)` inserts a row or, on a conflict over
  `{"target": [cols]}`, updates every other column, or the ones `"update"`
  names. It returns the row.
- `insert-many(table, rows)` writes up to 500 rows that name the same
  columns, in one statement, so it writes all of them or none. More than 500 is
  `ERR_DB_BATCH_TOO_LARGE` (-82), refused before anything is written.

The SDK now binds the structured calls as `trovato_sdk::host::db::{select,
insert, update, delete, upsert, insert_many}`; it bound none of them before.
They go through the plugin's allowlist and the protected floor, as before.

Translations (`item-api`):

- `list-translations` returns up to 100 items' translations, for the items
  the user may view, with the field tier applied to `fields`.
- `save-translation` and `delete-translation` are `ItemService` writes. They
  need `edit` access, or `translate content` and `view`. The field write gate
  applies, `tap_item_update` fires on the other plugins, and the item cache
  drops the item.
- A plugin no longer needs `item_translation` in `db_tables`.
- The admin translation screen now saves and removes through the same
  methods. A translation saved or removed there now fires `tap_item_update`,
  and drops the item from the cache, which it did not do before.

Contract: the new functions (`db.upsert`, `db.insert-many`,
`item-api.list-translations`, `save-translation`, `delete-translation`), their
SDK bindings, `ItemTranslation` and `ERR_DB_BATCH_TOO_LARGE` are **additive**.
No existing WIT signature changed. An array `where` was refused before, so
accepting it changes no call that worked. That an admin translation write
now fires `tap_item_update` is a **behavior change** (see `UPGRADING.md`).
