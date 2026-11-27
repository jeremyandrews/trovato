The gather row author query count test counts every statement on every run.
It failed now and then in CI with "0 for one author, 6 for twenty" and passed
on a rerun.

The test counted statements with a subscriber scoped to the gather's future
(`with_subscriber`), in a test binary that installs no global subscriber.
tracing starts with its maximum level off, so sqlx registers its `sqlx::query`
callsites only on the first statement after the first capture is created.
tracing-core caches each callsite's interest when it registers, and while
exactly one dispatcher exists it asks only the default subscriber of the
thread the callsite fired on. When that first statement came from another
thread (the shared app's cache generation watcher or cron scheduler, or a
concurrent test), that thread's default was the no-op subscriber, the interest
was cached as "never", and the first capture saw nothing. Creating the second
capture registered a new dispatcher, which rebuilt every interest, so it
counted normally and the two counts differed.

The test now calls `scoped_subscribers_see_every_callsite` before it captures,
the helper the other scoped query counters already used: a global no-op
subscriber keeps a second dispatcher alive, so every interest is asked of all
dispatchers and the capture's interest wins. The test also fails as a setup
error if a capture counts no statements, rather than reading a broken
subscriber as a query count regression; `gather_tile_test`'s query count gets
the same guard.
