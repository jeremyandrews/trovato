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
| One shared directory | Uploads and their image derivatives, and the Pagefind index | `UPLOADS_DIR` and `PAGEFIND_OUTPUT_DIR` pointing into it |

The shared directory is a filesystem every server mounts read-write: a
Kubernetes `PersistentVolumeClaim` with access mode `ReadWriteMany`, an NFS
export, or Amazon EFS. Object storage is not supported. Mount the volume one
level above the directories Trovato writes, for example at `/shared`, and set:

```sh
UPLOADS_DIR=/shared/uploads
PAGEFIND_OUTPUT_DIR=/shared/pagefind
```

Each server checks the uploads directory when it starts: it writes, reads back
and deletes a uniquely named file there, and refuses to start, naming the
directory, if it cannot. A broken mount fails the deploy, not the first
upload.

The Pagefind index is rebuilt by whichever server's cron run takes the lock.
It builds beside `PAGEFIND_OUTPUT_DIR`, in the same parent directory, then
renames the old index aside and the new one into place. Every server serves
`/static/pagefind/` from that directory. This swap has been tested on a Docker
volume only; on NFS or EFS, check that a rename within one directory is atomic
before relying on it, and keep `PAGEFIND_OUTPUT_DIR` below the mount point
rather than at it, because a mount point cannot be renamed.

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
