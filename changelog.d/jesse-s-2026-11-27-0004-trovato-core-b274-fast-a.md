An agent iterating on a change had no fast path through the integration
suite beyond running every test binary, each of which relinks the whole
kernel, and `config_storage_test` additionally built a full-pool `TestApp`
per test instead of reusing the suite's shared one, so a higher thread count
than CI's pinned value asked Postgres for more connections than it allows and
failed with pool timeouts for a reason the test itself gave no sign of.

`scripts/test-changed.sh` now runs only the unit tests and integration
targets a change set actually touches. `config_storage_test`'s tests now
share the suite's one `TestApp` like the rest of the suite, and a
directly-built `TestApp` defaults to a smaller connection pool than the
shared one, so several can coexist without exhausting the server. A WASM
compilation cache for the plugin runtime and a parallel, multi-lane full-suite
runner were also built and measured, and both were reverted: the cache's
payoff could not be isolated from other changes within one measurement
session, and the lanes surfaced real resource-contention flakiness on top of
a couple of pre-existing, unrelated test-interference bugs the session found
but did not fix. See `docs/performance/test-suite.md` for the measurements
and the full account of what was kept and what was not.
