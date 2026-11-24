A pull request's CI run finishes in AFTER_WALL minutes of wall clock instead of
42, every test still runs with the same assertions, and a budget in CI keeps it
there. Three causes set the old number, none of them a slow test.

Coverage ran on every pull request, instrumented and one test at a time, for
42 minutes, ten longer than any other job, and nothing gated on the number it
produced. It now runs on pushes to `main` and by hand.

Shard 1 of the Test job carried every unit and doc test on top of its third of
the integration targets, which put it ten minutes behind the other two. The
unit and doc tests are now one part per shard, and the integration targets are
partitioned longest first by their measured time (`.config/ci-test-weights.txt`)
instead of by index, so the three shards finish together. That partition is
free to move a target only because every target now passes alone on a fresh
database: `language_navigation_test` did not, because it relied on an earlier
target having migrated the translation plugin's table.

And every test ran serialised, `--test-threads=1`, in every job. The unit tests
now run under cargo-nextest, one process each and all at once, which isolates
the environment variables the single thread was protecting. The integration
targets stay under libtest, because each nextest process would rebuild the
whole test application (thirteen times the CPU for `front_page_test`), but run
their tests in parallel inside each target, four at a time, as a local
`cargo test` always has. Parallel tests found the races the single thread hid,
and each is fixed at its cause in test code: a lock released before the
test's deferred cleanup restored what it guarded (`common::hold_until_cleanup`
now holds it through cleanup), raw writes that skipped a cache a concurrent
request refilled, plugin tables created concurrently without a lock, and a
tracing capture that missed events because another thread registered the
callsite first (`common::scoped_subscribers_see_every_callsite`).

argon2 and Cranelift now build optimized in the dev profile (`Cargo.toml`).
Most of a test's CPU was spent in them unoptimized; optimized, the same targets
use about a third of it. The workspace's own crates build as before.
The Cluster job, 26 minutes of which 0.6 were tests, builds its image on the
dependency layer `docker-publish` already caches (the Dockerfile now builds the
dependencies in their own layer) and builds the test binary at the same time.

The budget: a test over 30 s fails CI unless `.config/slow-tests.txt` names it
with its measured time and a reason, and an allowlisted one fails at twice its
recorded time. Each Test shard and the Cluster job has a timeout set at its
expected time plus half. `docs/testing-ci.md` describes the split, where new
tests go, and how to remeasure.
