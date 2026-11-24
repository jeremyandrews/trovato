# Upgrading and rolling back

How to move a running site to a new release, and back. The rule every
migration follows (expand, then contract) is in
[UPGRADING.md](../../UPGRADING.md#upgrading-with-no-downtime-and-rolling-back);
the design is [docs/design/upgrades-and-probes.md](../design/upgrades-and-probes.md).

Read the new release's `UPGRADING.md` entry first. If it names a migration as
**incompatible**, that release needs planned downtime and cannot be rolled back
by image. Stop every server, run `trovato migrate`, then start the new release.

## On one server: nothing changes

With the default settings (`MIGRATE_ON_START` unset, or `true`):

1. Stop the server.
2. Install the new binary (or `docker pull` the new image).
3. Start it. It applies pending migrations, kernel then plugins, and serves.

To see what it will do before you start it:

```sh
trovato migrate --dry-run
```

**Rolling back:** install the previous binary and start it. It accepts the
migrations the newer release applied and serves. Do not touch the database.

`systemd`, `docker stop` and Ctrl-C stop the server as fast as they always
have: there is no drain unless `SHUTDOWN_DRAIN_SECS` says so.

## On Kubernetes

The example manifests are in [deploy/kubernetes/](../../deploy/kubernetes/):
a migration Job, the Deployment (`MIGRATE_ON_START=false`, liveness on
`/health/live`, readiness on `/health/ready`, a `preStop` pause,
`terminationGracePeriodSeconds: 45`, `maxUnavailable: 0`) and a Service.

### Upgrade

1. Read the release's `UPGRADING.md` entry.
2. Optionally, see what will be applied. This has to run the **new** image,
   because a running pod's binary does not know the new migrations:

   ```sh
   sed -e 's/0.107.0/0.108.0/g' -e 's/name: trovato-migrate-/name: trovato-dry-run-/' \
       -e 's/"migrate", "--lock-timeout", "10"/"migrate", "--dry-run"/' \
       deploy/kubernetes/migrate-job.yaml | kubectl apply -f -
   kubectl wait --for=condition=complete --timeout=5m job/trovato-dry-run-0.108.0
   kubectl logs job/trovato-dry-run-0.108.0
   ```

3. Run the migration Job for the new release, and wait for it. The old pods
   keep serving throughout, because every migration keeps them working.

   ```sh
   sed 's/0.107.0/0.108.0/g' deploy/kubernetes/migrate-job.yaml | kubectl apply -f -
   kubectl wait --for=condition=complete --timeout=15m job/trovato-migrate-0.108.0
   ```

   If it fails, read `kubectl logs job/trovato-migrate-0.108.0`. A failed
   migration rolls back its own transaction and the old pods are unaffected.
   A `lock_timeout` failure means a long query held the table; the Job retries
   by itself (`backoffLimit: 3`).

4. Roll out the new image:

   ```sh
   kubectl set image deployment/trovato trovato=ghcr.io/jeremyandrews/trovato:0.108.0
   kubectl rollout status deployment/trovato
   ```

   Each new pod becomes ready only once Postgres and Redis answer and its
   plugins are loaded. Only then does an old pod receive SIGTERM. It reports not
   ready at once, keeps serving through the `preStop` pause and
   `SHUTDOWN_DRAIN_SECS`, finishes its in-flight requests, and exits.

A pod that starts before step 3 has finished does not crash loop. It answers
`/health/live` 200 and `/health/ready` 503, notices within two seconds of the
Job finishing, and becomes ready once it has loaded its plugins.

### Roll back

Redeploy the previous image. Never roll back the schema.

```sh
kubectl rollout undo deployment/trovato
kubectl rollout status deployment/trovato
```

The previous image runs on the newer schema. When the fix ships, upgrade again
from step 1. Its migrations are already applied, so the Job finds nothing to do.

### Probes

| Path | 200 when | Use for |
|---|---|---|
| `/health/live` | the process answers (nothing else is checked) | `livenessProbe`, Docker `HEALTHCHECK` |
| `/health/ready` | Postgres and Redis answer, the schema is ready, plugins are loaded, not draining | `readinessProbe`, load balancer health checks |
| `/health` | Postgres and Redis answer (unchanged) | existing monitors |

Each answers `{"status": "..."}` to an anonymous caller, at most one dependency
check a second however often it is asked, with no session and no rate limit.
The details (per-dependency booleans, pool statistics, the services and circuit
breaker report) go to a caller on loopback with no forwarding header, or to one
sending `Authorization: Bearer $HEALTH_TOKEN`. A proxy in the same pod that
connects from loopback without adding `X-Forwarded-For` would make every
visitor look local, so do not route `/health` through one. A service mesh
sidecar (Istio, Linkerd) does exactly that: in a meshed pod, keep `/health`
out of the mesh or accept that every caller sees the details. Make
`HEALTH_TOKEN` long and random (`openssl rand -hex 32`): the probes have no
rate limit.

```sh
kubectl exec deploy/trovato -- curl -s localhost:3000/health
curl -s -H "Authorization: Bearer $HEALTH_TOKEN" https://example.com/health
```

### Settings

| Setting | Default | Meaning |
|---|---|---|
| `MIGRATE_ON_START` | `true` | apply pending migrations when `serve` (or another subcommand) starts |
| `SHUTDOWN_DRAIN_SECS` | 5 under Kubernetes, 0 elsewhere | after SIGTERM, keep serving this long while reporting not ready |
| `SHUTDOWN_TIMEOUT_SECS` | 30 | then wait this long for in-flight requests (only bounded when a drain is set) and background work |
| `HEALTH_TOKEN` | unset | bearer token that sees the health details from anywhere |

Keep `terminationGracePeriodSeconds` above `preStop` + `SHUTDOWN_DRAIN_SECS` +
`SHUTDOWN_TIMEOUT_SECS` (one budget for in-flight requests, background work and
closing the pool) plus a few seconds.
