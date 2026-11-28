# Running Trovato on more than one server

Trovato runs the same way on one server or many. Nothing switches a site into
a cluster mode: two kernels pointed at the same Postgres, the same Redis and
the same shared directory behave as one site behind a load balancer, with no
sticky sessions. A single server needs none of this and none of its settings.

## What the servers share

| What | Why | Setting |
|---|---|---|
| One Postgres | All content, configuration, users, plugin status and the cron lock | `DATABASE_URL`, the same on every server |
| One Redis | Sessions, rate limit counters, the shared cache, queues, cron locks and the cache generations | `REDIS_URL`, the same on every server |
| One file store | Uploads and their image derivatives | `UPLOADS_DIR` on the shared directory, or an S3 bucket: `FILE_STORAGE=s3` and the `S3_*` settings, the same on every server |
| One shared directory | The Pagefind index, and uploads when they are not in a bucket | `PAGEFIND_OUTPUT_DIR` (and `UPLOADS_DIR`) pointing into it |

The shared directory is a filesystem every server mounts read-write: a
Kubernetes `PersistentVolumeClaim` with access mode `ReadWriteMany`, an NFS
export, or Amazon EFS. Mount the volume one level above the directories
Trovato writes, for example at `/shared`, and set:

```sh
UPLOADS_DIR=/shared/uploads
PAGEFIND_OUTPUT_DIR=/shared/pagefind
```

Each server checks the uploads directory when it starts: it writes, reads back
and deletes a uniquely named file there, and refuses to start, naming the
directory, if it cannot. A broken mount fails the deploy, not the first
upload.

### Uploads in an S3 bucket

Instead of the shared directory, uploads and their image derivatives can live
in any S3 compatible bucket (AWS S3, MinIO, or another provider's S3 API):

```sh
FILE_STORAGE=s3
S3_BUCKET=example-files
S3_REGION=eu-south-1
# S3_ENDPOINT=http://minio:9000 and S3_ALLOW_HTTP=true for a local MinIO
```

Every server reads and writes the same objects, so a derivative one server
made is served by all of them, and only the Pagefind index still needs the
shared directory. Files are still served only through the kernel's `/files/`
routes, which check access first: keep the bucket private. Each server checks
the bucket when it starts (writes, reads back and deletes a uniquely named
object). It refuses to start, naming the bucket, on a definite answer (no
such bucket, access denied, wrong region); a store that does not answer is
logged and the server starts, so check the status report after a rollout. An existing
directory is copied in with `trovato files migrate-to-s3`
([command line reference](../cli-reference.md#trovato-files-migrate-to-s3)).
The design is [docs/design/production-hosting.md](../design/production-hosting.md).

Each server's built-in cron scheduler (`cron_interval_secs` in site settings,
default 300 seconds) calls cron on its own cadence, and an external caller to
`/cron/{key}` may still poke any server at any time — every one of them, the
scheduler included, takes the same Redis lock before running, so a scheduled
run on one server and an external poke to another, landing in the same
window, never both run; the second is told another instance already is. A
scheduler also skips its turn when cron finished anywhere less than an interval
ago, so several servers together run cron about once per interval, not once
each. No cron sidecar is needed on any server.

The Pagefind index is rebuilt by whichever server's cron run takes the lock.
The kernel writes it itself, in Pagefind 1.3.0's format, so no `pagefind`
binary is needed on any server or in the image. It builds the new index in a
directory beside `PAGEFIND_OUTPUT_DIR` (the parent must be writable), then
moves its files into `PAGEFIND_OUTPUT_DIR` one by one: the new index files,
whose names are content hashes and so never overwrite a file a reader is
using, then `pagefind-entry.json` last, with a rename. A browser that loaded
the previous index keeps finding its files, because a file the new index no
longer uses is deleted only after 24 hours. Every server serves
`/static/pagefind/` from that directory. Only renames of single files within
`PAGEFIND_OUTPUT_DIR` need to be atomic; when the build directory is on a
different filesystem, each file is copied to a temporary name there first.

Everything else every server needs is in the image: plugins, templates and
static files. Give every server the same image and the same environment.

## How a change on one server reaches the others

Each server keeps a few caches in its own memory: permissions, users, items,
field access answers, content types, categories, gather queries, redirects,
interface translations and config menus. A write clears its own server's copy
at once and adds one to that cache's counter in the Redis hash `cache:gen`.
Every server reads the hash every 250 ms and clears any cache whose counter
changed. So a role revoked, a user blocked or an item edited on one server
holds on every server within about 250 ms, plus the request that was already
running.

A write that does not go through a server (SQL run by hand, a restore, a
script) bumps nothing, so servers keep serving their cached copies until each
cache's time limit (`CACHE_TTL`, and five minutes for users, items and
categories). To have them serve it at once, without a restart:

```sh
trovato cache clear                 # every kind
trovato cache clear --kind items    # or only the ones the write touched
```

It runs anywhere with the site's `REDIS_URL`, and returns once every server
has had time to clear (one second) ([command line reference](../cli-reference.md#trovato-cache-clear)).
`trovato config import` and the other commands that write end with the same
clear.

If Redis cannot be read, each server logs one warning and its caches fall back
to their own time limits until Redis answers again. When Redis restarts the
hash starts again from nothing, which every server reads as a change and
clears everything once. Sessions live in Redis too, so a Redis that loses its
data logs everyone out.

Rate limits are counted in Redis: logins, the API, chat, the assistant, and
each AI provider's requests per minute. A limit applies to the site, not to
each server.

## What stays per server, on purpose

**AI circuit breakers.** Each server has its own breaker for each kind of AI
request. A provider that starts failing opens the breaker on each server after
that server has seen enough failures, so with several servers a few more
requests fail before every breaker opens. Sharing them would put a Redis round
trip in front of every AI call to save those few requests.

**The enabled plugin set.** A server loads the enabled plugins once, when it
starts. Enabling or disabling a plugin, on the admin page or with
`trovato plugin enable` or `disable`, changes only the stored status. The
admin page says so, and marks each plugin whose change is still waiting.

To apply it, restart every server, one at a time:

1. Make the change on any server.
2. Restart one server and wait until `/health/ready` answers 200 and the load
   balancer sends it traffic again.
3. Repeat for each of the others.

Until the last one restarts, servers disagree about that plugin: its routes,
menus and taps exist on some and not others. Keep the window short.

A plugin's `tap_install` runs once per site, not once per server: the first
server to start after the plugin is enabled claims it in `plugin_status`
before running it, and the others skip it.

## Testing it locally

The repository has a two server harness: two kernels from one image behind
HAProxy with round robin and no stickiness, one Postgres, one Redis and one
shared volume.

```sh
docker compose -f docker-compose.cluster.yml up -d --build
TROVATO_CLUSTER_URL=http://localhost:8095 \
  cargo test -p trovato-kernel --test cluster_test -- --test-threads=1
docker compose -f docker-compose.cluster.yml down -v
```

The proxy listens on port 8095 and Postgres on 56432 (`CLUSTER_PROXY_PORT` and
`CLUSTER_PG_PORT` change them). A request with the header `X-Trovato-Node: a`
or `b` goes to that server, and every response names the server that answered
in `X-Trovato-Served-By`. Without `TROVATO_CLUSTER_URL` the cluster tests skip
themselves, so `cargo test --all` does not need the harness. CI runs it in the
`cluster` job.
