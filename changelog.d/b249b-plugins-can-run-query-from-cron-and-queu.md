A plugin's `query` host call now works from `tap_cron` and `tap_queue_worker`
as it does in a request. The background services builder,
`CronService::background_services`, bound the item service for `item-api` but
never the gather service, so `query` returned `ERR_NO_SERVICES` (-10) from cron
and queue workers even for a plugin holding `item_background`. A plugin that
needed to list items from cron fell back to `query_items`, which returns whole
items and could overflow the SDK's result buffer on large ones. `CronService`
now carries the site's one `GatherService` and binds it next to the item
service, and the `tap_install` dispatch at startup binds it too. The access
rules are unchanged: without `item_background`, a content `query` from either
background path is still refused with `ERR_ITEM_BACKGROUND_DENIED` (-61), and
with it the viewer is the same background principal `item-api` uses.
