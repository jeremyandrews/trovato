A plugin's item write is now the write a person makes. `save-item` and
`delete-item` called the `Item` model directly, a deliberate shortcut to keep a
tap from dispatching taps, and it skipped everything the item service does
around a write: the presave, insert, update, index and delete taps, the field
and publish write gates, file reference sync and the item cache, so a page kept
serving the old title for up to `CACHE_TTL_ITEMS` after a plugin changed it.
Both functions now call `ItemService::create`, `update` and `delete`; the access
decision and the gates are the service's, and the host keeps only the create
permission check the item routes also make before calling the service.

Re-entrancy is bounded by a task-local stack of the plugins whose write tap is
running: a write made from inside one fires its taps on every other plugin, and
a write nested four deep is refused with the new `ERR_ITEM_WRITE_DEPTH` (-63). A
background call from a plugin holding `item_background` goes through the service
as the background principal, which now passes `check_access` beside the
administrator, the way it already passed both write gates. `delete-item` on an
id that names no item returns the new `ERR_ITEM_NOT_FOUND` (-62) instead of 0,
`save-item` takes `language` on a create, validated against the site's
languages, and the separate embed enqueue for plugin saves is gone because the
service's own indexing now covers them.

The SDK binds all four `item-api` functions as `trovato_sdk::host::get_item`,
`save_item`, `delete_item` and `query_items`, and `host_errors` documents every
code they return. The plugin documents now describe `tap_item_insert`,
`tap_item_update` and `tap_item_delete` as notifications whose output is not
read, and `tap_item_presave` as the tap that changes a write (BL-88, documents
only).

Contract: the WIT signatures are unchanged and the SDK bindings and two error
codes are additive. What a plugin observes from `save-item` and `delete-item`
changes (taps fire, the gates refuse, a missing delete is -62), which is a
change to an existing host function's behaviour and so frozen surface under the
version map's rules, taken in a minor release while the project is 0.x.
