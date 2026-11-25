A plugin's `item-api` calls work from `tap_cron` and `tap_queue_worker` again.
Since B68.2 (`db45018`) routed plugin writes through `ItemService`, every
`save-item` and `delete-item` from either path answered `ERR_NO_SERVICES` (-10)
and wrote nothing, whatever the plugin's `item_background` said; the
translation writes added by B80.4 did the same. The root cause was that `CronService` builds the
services for both background paths in one place, `background_services`, and
that builder never bound the item service `CronService` already holds; the
`item-api` host functions found no service to decide with and refused before
the capability was looked at. The request path binds it in `AppState::new`, and
every kernel test of background item writes bound it by hand, so nothing in the
suite exercised the services cron actually builds. `background_services` now
binds it, through the same once-only, weak `set_item_service` the request path
uses, and `crates/kernel/tests/background_item_api_test.rs` runs a fixture
plugin, `test_item_background`, through the real `AppState`, the real cron
cycle and the real queue drain, creating, updating and deleting an item from
each. A plugin without `item_background` is still refused there, with
`ERR_ITEM_BACKGROUND_DENIED` (-61).

`tap_install`, which runs at boot in a task started before the item service
exists, now waits for it and binds it too, so a plugin's `item-api` calls from
`tap_install` are decided as on a request rather than refused.

A dispatch path that carries no item service now answers the new
`ERR_ITEM_SERVICE_UNAVAILABLE` (-64), and the server log names the plugin and
the call, instead of the general `ERR_NO_SERVICES` with nothing logged. Every
server path carries one; the code is there so that a wiring fault like this one
says what it is.
