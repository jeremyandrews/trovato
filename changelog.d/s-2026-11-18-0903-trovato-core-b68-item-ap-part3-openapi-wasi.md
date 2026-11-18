`/api/openapi.json` now describes the routes the server serves, plugin routes
included. It was a hand-written list rebuilt on every request, so no plugin
route ever entered it, and three of its paths (`/api/v1/items`,
`/api/v1/items/{id}`, `/api/v1/categories`) were never routed at all. The
document is built once at startup from the kernel's route metadata and every
enabled plugin's `api` menu entries (method, path, title and required
permission), read through the same rule the plugin router mounts from; the
three paths are now the routed `/api/items`, `/api/item/{id}` and
`/api/categories`, and a test requests every listed path and fails on a 404 or
405.

A plugin built with Rust after 1.96 runs. The kernel's WASI stubs did not
include `clock_time_get`, which those builds import, and loading never noticed
because the import check skipped WASI and instantiation happens per dispatch,
so every tap failed with `unknown import` while the loader reported success.
`clock_time_get` now returns real time for the realtime and monotonic clocks,
and loading pre-instantiates the module against its linker, so any import the
kernel does not provide, WASI or not, fails that plugin's load with a message
naming it (BL-118).

Contract: additive. The WASI stub is a new import the kernel satisfies, and the
OpenAPI document is a kernel route, not plugin contract. A plugin that loaded
before with an unresolvable import now fails to load instead of failing every
dispatch; it could never have run.
