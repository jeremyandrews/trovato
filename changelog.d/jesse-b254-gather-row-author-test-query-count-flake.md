`gather_row_author_test`'s query count test, which checks that a gather costs
the same number of statements for one author as for twenty, could see zero
statements and pass for the wrong reason. Its `Events` layer was installed only
with `.with_subscriber(..)`, in scope for one future on one task. The first time
the `sqlx::query` callsite fires in the whole test binary with no dispatcher
installed, tracing caches its interest as never, and a later scoped subscriber
is never asked again, so the count comes back zero and the assertion that zero
equals zero passes. The fix installs a global no-op subscriber once per binary
before any scoped capture runs, the same fix already in place in
`gather_tile_test`, `menu_tree_test`, `field_access_rest_test` and
`alias_resolution_test`, and adds an assertion that the count is at least one
before comparing, so a broken subscriber fails as a setup error instead of
looking like a pass.
