A two node test harness: `docker-compose.cluster.yml` runs two kernels from
the release image against one Postgres, one Redis and one shared volume, behind
HAProxy balancing round robin with no stickiness. A request carrying
`X-Trovato-Node: a` or `b` is pinned to that node, in proxy configuration only,
so a test can write on one node and read on the other. Thirteen scenarios in
`crates/kernel/tests/cluster_test.rs` drive it over HTTP when
`TROVATO_CLUSTER_URL` is set and skip otherwise, and a new `cluster` CI job
builds the image, brings the harness up and runs them, printing both kernels'
logs on failure.

Sessions, form tokens, passkey challenges, uploads on the shared volume,
blocking a user and the cron lock already work across nodes. Nine scenarios
fail today and are ignored, each naming the change that fixes it: in-process
caches invalidated only on the node that wrote (roles, items, types, terms,
gather queries, redirects, locale strings, the cache layer's local tier, and an
import run from another process), chat limits counted per node, a
`tap_install` that two kernels booting together both run, a plugin toggle
applied live on one node only, and a Pagefind index written to one node's
disk. The CI test shards leave `cluster_test` out of their round robin
partition, where it would only skip itself.
