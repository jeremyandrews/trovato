# Upgrades With No Downtime: Migrations, Probes and Draining

**Status:** Design, built in BG10 (`bg10-safe-migrations`).
**Read against:** `origin/main` at `4ec50e1`.

## Goal

Trovato upgrades on Kubernetes with no downtime. Schema migrations run as their
own step before new pods start. Every migration keeps the previous release
working, so a rollback is just redeploying the previous image. Pods expose
separate liveness and readiness probes and drain cleanly on shutdown. A single
server install keeps working exactly as it does today.

## What `main` does today (rechecked)

Each fact in the brief was checked against `4ec50e1`:

1. `AppState::new` runs kernel migrations on every boot
   (`crates/kernel/src/state.rs:341`, `db::run_migrations`, which wraps
   `sqlx::migrate!("./migrations")` in `crates/kernel/src/db.rs`). Plugin
   migrations follow at `state.rs:546-557` (`plugin/migration.rs`). The
   `plugin`, `config` and `user` subcommands also migrate at start
   (`main.rs:586, 625, 705`). There is no `trovato migrate`. The subcommands are
   `serve`, `plugin new|list|install|migrate|enable|disable`,
   `config export|import [--dry-run]` and
   `user reset-password|role-add|role-remove|roles` (`main.rs:77-178`).
2. **Locking, checked in the source, not assumed.** sqlx 0.9.1's
   `Migrator::run` takes a Postgres **session** advisory lock
   (`pg_advisory_lock`, key derived from the database name) for the kernel
   migrations (`sqlx-core-0.9.1/src/migrate/migrator.rs:236`,
   `sqlx-postgres-0.9.1/src/migrate.rs:186`). Two caveats matter:
   - the lock is taken on a connection checked out of *our pool*. If a
     migration fails, `run` returns before `unlock`. That connection goes back
     to the pool still holding the session lock, and every later migrator
     blocks until the pool recycles it;
   - **plugin migrations take no lock at all.** Two processes both read
     `plugin_migration`, both find the same pending files, and the second fails
     on the duplicate `CREATE TABLE` or on the `(plugin, migration)` primary key.
3. sqlx refuses a database that holds applied migrations the binary does not
   know (`MigrateError::VersionMissing`, `validate_applied_migrations`). So an
   older binary started on a newer schema exits at boot, and a rollback fails
   today.
4. Migrations only move forward (`ROADMAP.md:191` "Migration rollback";
   `docs/BACKLOG.md` BL-80, open). Nothing says a migration must keep the
   previous release working.
5. `/health` (`routes/health.rs`) runs `SELECT 1` and a Redis `PING`, answers
   200 or 503, and returns **pool statistics and the full services and
   circuit breaker report to every caller, anonymous included**. It sits inside
   the whole middleware stack, so every probe uses the visitor `page` rate-limit
   bucket for its IP, and every call runs both checks plus a second pair inside
   `health_report`, with no caching. There is no liveness/readiness split.
6. SIGTERM and SIGINT trigger axum's `with_graceful_shutdown` (`main.rs:512`,
   `544-569`). Nothing waits before the listener closes, and the process never
   reports itself not ready before it closes.
7. `Dockerfile` uses `HEALTHCHECK curl -f /health` and `CMD ["./trovato"]`. The
   tree has no Kubernetes manifests. Rolling restarts are described by hand in
   `docs/deployment/multi-server.md:76-81`.

BG4 (`docs/design/hosted-tenancy.md`) has not merged host resolution:
`middleware/tenant.rs` has no host mode. There is nothing to exempt the probes
from yet. This design gives BG4 one function to exempt them with
(`routes::health::is_probe_path`).

## Decisions

### 1. `trovato migrate`

```
trovato migrate              # apply pending kernel then plugin migrations
trovato migrate --status     # list applied, pending and unknown (newer) migrations
trovato migrate --dry-run    # list what would be applied; change nothing
          [--lock-timeout <SECONDS>]   # default 10, 0 = wait forever
```

- **One lock for the whole run.** The command opens one dedicated connection
  (not a pooled one) and takes `pg_advisory_lock(TROVATO_MIGRATE_LOCK)`. That is
  a fixed key of our own, separate from sqlx's key. Kernel migrations
  (`MIGRATOR.run` on that same connection), then plugin migrations in
  dependency order, all run on that connection. Then the lock is released and
  the connection closed. If anything fails, the connection is dropped, Postgres
  ends the session, and the lock goes with it. A failed run can never leave a
  lock behind in a pool. This fixes caveat 2a.
- **Safe from several places at once.** A second process blocks on the lock.
  Once it gets the lock it finds nothing pending and exits 0. Each migration is
  applied exactly once.
- **Idempotent.** Running it on an up-to-date database does nothing and exits 0.
- **Nonzero exit on any failure**: connection, lock, a failed migration, an
  edited applied migration (checksum mismatch), a dirty migration, or an
  unknown applied migration marked incompatible (see 3).
- **`lock_timeout` is set after the advisory lock is held, not before.**
  Postgres applies `lock_timeout` to advisory lock waits too. Set first, it would
  make the second of two concurrent runs fail instead of wait. Set after, it
  limits how long a migration's DDL can sit in the lock queue on a busy table.
  Without it, an `ALTER TABLE` waiting behind a long read blocks every later
  read of that table, which takes the site down. On timeout the migration's
  transaction rolls back, the command exits nonzero, and the Kubernetes Job
  retries it (`backoffLimit`).
- **Which plugins.** The same set `serve` would run: plugins enabled in
  `plugin_status`, plus discovered plugins not yet in `plugin_status` that
  `serve` would auto-enable (`default_enabled`, not in `DISABLED_PLUGINS`). The
  command does not write `plugin_status`. `serve` still does that at boot, as
  today.
- **Redis is not needed.** The Job only talks to Postgres.
- `--status` and `--dry-run` exit 0 when the report could be read, whatever it
  says. They take no lock and write nothing, so they can never block a running
  migration.

The existing `trovato plugin install|migrate|enable` run their plugin
migrations under the same lock. `db::run_migrations`, which the tests call,
takes it too.

### 2. Boot: `MIGRATE_ON_START`

`MIGRATE_ON_START` (default `true`) decides whether `serve` and the other
subcommands migrate at start.

- **`true` (default, single server): nothing changes.** `serve` migrates kernel
  and plugin migrations where it always has, now under the lock. One deliberate
  difference: an applied migration the binary does not know, but which is not
  marked incompatible, is accepted instead of refused (sqlx `ignore_missing`,
  gated on our own check). That only changes a case that used to exit at boot.
  It is what lets a single server roll back by reinstalling the previous binary.
- **`false` (hosted): `serve` never changes the schema.** Before building the
  application it reads the schema state (`migrate::check`). If anything it
  needs is pending (kernel or enabled-plugin migrations), or Postgres cannot be
  reached yet, it does not exit, so there is no crash loop. It binds the port and
  serves only the three probes: live 200, ready 503, `/health` 503. It checks
  again every 2 seconds. When the schema is ready it stops the probe server,
  builds the full application on the same socket, and starts serving.
  SIGTERM while waiting exits cleanly. The other subcommands refuse to run on a
  schema that is behind and say "run `trovato migrate` first".
- **Unknown applied migrations** (in the database, not in this binary) are
  accepted unless their name marks them incompatible (see 3). This is what lets
  the previous image run on the new schema.
- **The check is run once, at boot.** Readiness does not re-read the schema on
  every probe. Under the rule in section 3, a running pod's code cannot become
  incompatible with forward migrations, so re-reading would only add load. A
  migration that does break the previous release is the documented exception:
  it needs a planned stop of the old pods (`UPGRADING.md`).

### 3. The compatibility rule: expand, then contract

**Every migration must leave the previous release working.** The previous
release's pods are still serving while the migration Job runs, and they are
what a rollback redeploys.

Allowed at any time ("expand"):
- new tables, new views, new functions;
- new nullable columns, or columns with a `DEFAULT`;
- indexes on existing tables built `CONCURRENTLY` in a file whose first line is
  `-- no-transaction` (sqlx's directive; `CONCURRENTLY` cannot run in a
  transaction). Plain `CREATE INDEX` is fine on a table the same file creates;
- inserts and updates of rows the previous release reads the same way.

Only in a later release, once no released code reads or writes the object
("contract"):
- `DROP TABLE`, `DROP COLUMN`, `DROP VIEW`, `DROP FUNCTION`, `DROP TYPE`,
  `DROP SCHEMA`;
- `RENAME` of a table or column;
- `ALTER COLUMN ... TYPE`, `SET NOT NULL`, `ADD COLUMN ... NOT NULL` without a
  `DEFAULT`, `ADD CONSTRAINT`, `TRUNCATE`;
- `CREATE INDEX` without `CONCURRENTLY` on a table the file does not create
  (it blocks writes for the whole build).

**A rename, done properly:** release N adds the new column and writes both;
release N+1 reads the new one; release N+2 drops the old one with a marker
naming N+1.

**Enforcement, mechanical and cheap.** `crates/kernel/src/migration_rule.rs`
is a pure function over a migration's SQL, about 150 lines with no parser
crate. Comments and string literals are stripped first, then the statements
are matched against the list above. A unit test runs it over every kernel
migration newer than the cutoff (`20261127160002`, the newest on `main` when
the rule was written; older files are history) and every plugin migration not
on a short grandfather list. The unit test runs in `./scripts/pre-commit-check.sh`
(as a named step) and in CI (the shard 1 unit test step, also named). A
refused statement passes only if the file carries

```sql
-- trovato:contract <release>: <what stopped using it>
-- e.g.  -- trovato:contract 0.108.0: item.legacy_flag unread since 0.108.0
```

`<release>` must be a version no newer than the workspace version, because the
release that stopped using the object has to exist already. The marker covers
the file, which makes the reviewer's question concrete: did `<release>` really
stop using it? The checker cannot answer that. A reviewer can, by looking at
that release.

**Two years from now** the rule still holds because it lives in a unit test
that fails on the pull request that breaks it, not in a document. The list of
statements is small and grows only by adding a pattern and a test.

**Incompatible migrations, the escape hatch.** If a change cannot keep the
previous release working, the file is named `<version>_incompatible_<desc>.sql`
(kernel) or `<NNN>_incompatible_<desc>.sql` (plugin). The name is what an older
binary can see: sqlx records the description (`incompatible ...`) in
`_sqlx_migrations`, and plugin file names are stored in `plugin_migration`. An
older binary refuses to become ready on such a schema, so a rollback across it
fails loudly instead of corrupting data. Such a release needs downtime, and
`UPGRADING.md` must say so.

**Backfills (data migrations).** A migration that rewrites rows runs inside the
Job's transaction while the site serves. The budget: **at most about one
second of row locks on any table the site writes**, measured against the
largest install's row count. Anything bigger is not a migration. It is an
expand migration (new nullable column) plus a batched backfill that runs from
cron or the queue in batches of at most 1000 rows, one short transaction each,
plus a later contract migration once the backfill is done. `lock_timeout`
(section 1) is the safety net, not the plan.

### 4. Proof of rollback in CI

A CI job, `rollback`, builds two debug binaries: this branch's and the
**previous** one. It runs the branch's `trovato migrate` on a fresh database,
installs the site with the branch binary, then starts the previous binary on
that schema and passes a smoke test: ready, an anonymous page answers 200, and
a login succeeds. The driver is a test in `zero_downtime_test.rs`
(`previous_release_serves_on_this_schema`), skipped unless
`TROVATO_PREVIOUS_BIN` names the previous binary.

**Which "previous".** The newest `v*` release tag that already contains this
change (`crates/kernel/src/migrate.rs` exists in it). **No such tag exists
yet:** `v0.104.0` and older all migrate at boot with sqlx's default
`ignore_missing = false`, so no released binary can start on a schema with one
more migration. So, until the first release containing this change is tagged,
the job builds the **merge base with `main`**, and says which it chose in its
log. For this pull request that is `main` at the merge base. The branch adds no
kernel migration, so this run proves the harness works and that the branch
does not break an old binary's boot. Once a release with this change is
tagged, the same job proves the real rollback for every later pull request.
The in-tree test `binary_without_newest_migration_serves_on_newer_schema` proves
the tolerant path itself today. It starts the branch binary on a schema that
holds a migration the binary does not know.

Both builds share one target directory and the dependency cache, so the job
compiles the workspace crates twice and the dependencies once.

### 5. Probes

| Path | Answers | Anonymous body |
|---|---|---|
| `/health/live` | always 200 while the process runs; no dependency is checked | `{"status":"alive"}` |
| `/health/ready` | 200 when Postgres and Redis answer, the schema was compatible at boot, plugins are loaded (true once the full app is serving), and the server is not draining; otherwise 503 | `{"status":"ready"}` / `{"status":"not ready"}` |
| `/health` | as today: 200 when Postgres and Redis answer, else 503 | `{"status":"healthy"}` / `{"status":"unhealthy"}` |

- **Liveness never looks at a dependency**, so a database blip takes pods out
  of rotation (readiness) and never restarts them (liveness).
- **Cheap and bounded.** Postgres and Redis are checked at most once per second
  per process, whatever the probe rate. The result is cached behind a
  single-flight lock: concurrent probes wait for the one check in progress and
  share it. Each check has a 2 second timeout, so a hung dependency cannot pile
  up waiters. `/health/live` touches nothing.
- **No session, cookie or visitor rate-limit bucket.** The probes stay in the
  router where `/health` is today, so the security headers still apply. They are
  exempted by one function, `routes::health::is_probe_path`, from the per-IP
  rate limiter (no bucket is touched), maintenance mode, the installer
  redirect, redirects, path aliases and language negotiation, the same way
  `/health` was already exempted from the last four. No handler reads the
  session, so no cookie is ever set. A probe never costs a Redis round trip
  beyond the cached `PING`.
- **Details only for a trusted caller.** Pool statistics, the services report
  and the per-dependency booleans go only to a caller that is either
  - connected from a loopback address with **no** `X-Forwarded-For`,
    `Forwarded` or `X-Real-IP` header. The no-header condition matters because a
    reverse proxy on the same host connects from loopback, and its visitors must
    not count as local; or
  - presents `Authorization: Bearer <HEALTH_TOKEN>`, compared in constant time,
    when `HEALTH_TOKEN` is set.

  Everyone else gets the status word only. Existing tests that read
  `postgres`/`redis` from `/health` run as a loopback caller, so they still see
  them.

### 6. Shutdown and draining

On **SIGTERM**:
1. Readiness turns false at once (`/health/ready` answers 503).
2. The resident queue runner stops taking new batches. A batch already claimed
   finishes, or, if the process is killed first, its claim lease
   (`locked_until`) expires and another process takes it. That is the "release"
   half.
3. The server keeps accepting and serving for `SHUTDOWN_DRAIN_SECS`.
4. It stops accepting, finishes in-flight requests for up to
   `SHUTDOWN_TIMEOUT_SECS` (existing setting, default 30), then the remaining
   background tasks stop (search analytics flush once more, as today), the pool
   closes, and the process exits.

`SHUTDOWN_DRAIN_SECS` defaults to **5 when running under Kubernetes**
(`KUBERNETES_SERVICE_HOST` is set, which the kubelet does in every pod), and
**0 otherwise**. With 0, the steps above are exactly today's shutdown, so a
single server stops as fast as it does now. **SIGINT** (Ctrl-C at a terminal)
never drains. Five seconds covers endpoint removal reaching kube-proxy and the
ingress controller in a normal cluster. The `preStop` pause in the manifest
covers the same window before the process even hears about it. The two
together make it hold for load balancers that watch readiness and for those
that watch endpoints.

`terminationGracePeriodSeconds` must exceed `preStop` + drain +
`SHUTDOWN_TIMEOUT_SECS`. The example uses 5 + 5 + 30 + 5 = 45.

The external cron trigger (`POST /cron/{key}`) is an HTTP request. It is an
in-flight request like any other, and cron's Redis lock already keeps it to
one process.

### 7. Docker and Kubernetes

- `Dockerfile` `HEALTHCHECK` uses `/health/live`. A container waiting for
  migrations is alive, not unhealthy.
- `deploy/kubernetes/`, plain YAML, no Helm: `migrate-job.yaml` (runs
  `trovato migrate`), `deployment.yaml` (`MIGRATE_ON_START=false`, liveness on
  `/health/live`, readiness on `/health/ready`, `preStop` sleep,
  `terminationGracePeriodSeconds: 45`, `RollingUpdate` with `maxUnavailable: 0`
  and `maxSurge: 1`), `service.yaml`, and `kustomization.yaml` only so
  `kubectl apply -k` works. A CI job validates them with `kubeconform`, a
  single static binary downloaded in one step.

### Settings added

| Setting | Default | Meaning |
|---|---|---|
| `MIGRATE_ON_START` | `true` | migrate at start (`serve` and subcommands) |
| `SHUTDOWN_DRAIN_SECS` | 5 under Kubernetes, else 0 | serve this long after SIGTERM while not ready |
| `HEALTH_TOKEN` | unset | bearer token that sees health details from anywhere |

## What this does not build, and why

- **Down migrations or schema rollback.** Out of scope (BL-80 stays open for
  it). A rollback is the previous image on the current schema.
- **A schema version table or compatibility floor.** The incompatible-name
  marker gives an older binary the one bit it needs, using tables that already
  exist.
- **Re-checking the schema on every readiness probe.** See section 2. It would
  cost one query per probe and buy nothing under the rule.
- **A Helm chart, an operator, autoscaling, a service mesh.** Out of scope; the
  manifests are examples.
- **A SQL parser crate for the rule.** The statements refused are few and
  keyword-shaped. A parser would be a dependency for a check that only has to
  be right about a dozen patterns. **No new crate** is added anywhere.
- **Startup probe.** Readiness already covers a slow boot. Liveness never
  fails during boot because the waiting server answers it.
- **Draining the queue to empty.** Shutdown finishes the current batch, not the
  queue. Leases cover the rest.

## Plugin API and security

No change to the plugin SDK, the WIT interface or any host function. The plugin
API is untouched. Security checks are strengthened, not weakened: `/health`
stops publishing pool statistics and the services report to the internet, and
the probes no longer touch the visitor rate-limit buckets. Leaving the buckets
removes no protection, because each probe costs at most one cached check per
second.

## Tests

In `crates/kernel/tests/zero_downtime_test.rs`, a new target named to sort
after every existing kernel target, so the CI shard assignment of existing
files does not move (`scripts/ci-test-shard.sh` partitions by sorted index):

- two `trovato migrate` processes started together both succeed and every
  migration is applied once;
- with `MIGRATE_ON_START=false` and an unmigrated database, `serve` answers
  live 200 and ready 503, leaves the schema untouched, then becomes ready on
  its own after `trovato migrate`;
- a binary without the newest migration starts and serves on a schema that has
  it; one marked incompatible keeps it not ready;
- `/health/live` stays 200 with Postgres gone; `/health/ready` goes 503;
- after SIGTERM, `/health/ready` is 503 within the drain while a slow
  in-flight request still completes;
- an anonymous `GET /health` reveals no pool statistics;
- the rollback smoke test (CI `rollback` job only).

Unit tests in `migration_rule.rs`: a sample `DROP COLUMN` fails without the
marker and passes with it, plus each refused pattern and each allowed one.
