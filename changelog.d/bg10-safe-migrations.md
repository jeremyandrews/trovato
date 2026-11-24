Schema migrations can run as their own step, and a release is rolled back by
redeploying the previous image. `trovato migrate` applies pending kernel and
plugin migrations under one Postgres advisory lock held on a connection of its
own, so several runs at once apply each migration exactly once and a failed run
cannot leave the lock behind in a pool. Before this, sqlx's lock covered the
kernel half only, and plugin migrations took no lock at all. `--status` and
`--dry-run` change nothing.

`MIGRATE_ON_START` (default `true`, today's behaviour) can be turned off, and
then `serve` never changes the schema. It waits, answering `/health/live` 200
and `/health/ready` 503, instead of crash looping, and becomes ready by itself
once the migration has run. A binary now accepts applied migrations it does not
know, unless one is named incompatible. They used to stop it at boot, which made
a rollback by image impossible. Every new migration must keep the previous
release working (expand, then contract). A unit test run by the pre-commit
script and CI refuses the statements that break the previous release (a drop, rename,
retype, `SET NOT NULL`, `DROP DEFAULT`, `TRUNCATE`, a `NOT NULL` column without
a default, a new constraint, a plain or unique index build on an existing table; see
`UPGRADING.md`), unless the file names the release that
stopped using the object. A CI job starts the previous binary on the schema the
change produces.

`/health/live` and `/health/ready` are new. `/health` no longer gives pool
statistics and the services report to anonymous callers, and none of the three
uses a session or a visitor rate-limit bucket. `/health` and `/health/ready`
check Postgres and Redis at most once a second; `/health/live` checks nothing.
On SIGTERM the server reports not ready at once, keeps serving for
`SHUTDOWN_DRAIN_SECS` (5 under Kubernetes, 0 elsewhere), and finishes
in-flight requests before it exits. Example Kubernetes manifests are
under `deploy/kubernetes/`.
