# Production Hosting on AWS: One Region, One Command

Status: decided (BG16). Part 1 (`bg16-s3-storage`, the S3 file store) is built
with this document. Parts 2 (`bg16-infra`) and 3 (`bg16-ops`) build the rest of
what it decides, in that order.

## Goal

Everything needed to stand trovato.blog up on AWS in one region with one
command once credentials exist: infrastructure as code for the cluster, managed
Postgres, Redis and S3; Trovato keeping files in S3 so any number of pods can
serve them; backups whose restore is proved by a drill; alerts that page only
for what a human must fix; and a public status page that stays up when the
platform is down. Nobody is woken at night for something that heals itself.

trovato.blog is a hosted blog network (BG4, `docs/design/hosted-tenancy.md`)
run from Italy by one operator, with Cloudflare in front (BG9 page cache, BG17
custom domains). Readers are served from Cloudflare's cache, so the app can
restart without readers noticing; writes and the admin need the app. Later work
adds more regions, one install per region, with blogs moved between them by
export and import, so nothing here assumes one region forever.

## What `main` does today (rechecked at `fe9bdfde`)

- Files: `FileStorage` (`crates/kernel/src/file/storage.rs`) had one
  implementation, `LocalFileStorage`. The `/files/{*path}` route
  (`routes/file.rs`) checks access on every request (`ItemService::file_access`)
  and sends `public, max-age=604800` only when an anonymous visitor would be
  allowed the same file, else `private, no-store`. Image style derivatives were
  written by `ImageStyleService::save_derivative` to `UPLOADS_DIR/styles/...`
  with `std::fs`, outside the trait. Several servers needed a `ReadWriteMany`
  volume (`docs/deployment/multi-server.md`: "Object storage is not
  supported"). A file the store did not hold was recognised by matching the
  words "not found" in the error text, which `LocalFileStorage`'s
  `failed to read file` context never contained.
- BG10 is merged (#263): `trovato migrate` (one lock, `--status`, `--dry-run`),
  `/health/live` and `/health/ready`, drain on SIGTERM, the expand-then-contract
  migration rule, and example manifests in `deploy/kubernetes/` (a migrate Job,
  a Deployment with probes, a Service, a `ReadWriteMany` claim for uploads).
- BG14 is merged: `/metrics` (prometheus-client) answers only loopback or
  `Authorization: Bearer <METRICS_TOKEN>`, with `http_requests`,
  `http_request_duration_seconds`, `db_query_duration_seconds`,
  `cache_hits`/`cache_misses`/`cache_sets`, `trovato_db_pool_*`,
  `trovato_circuit_breaker_email` and others; OTLP trace export.
- There is no infrastructure code, no backup or restore procedure, no alert
  rule and no status page anywhere in the repository.
- Postgres needs: `pgvector` if present (the embeddings migration enables it
  when it can and skips it otherwise), `pg_advisory_lock` for migrations
  (session level), and ICU collations, all of which RDS for PostgreSQL 16 has.
- Redis holds: sessions, rate limit counters, the shared cache and its
  generations (`cache:gen`), cron locks, replay keys and the legacy
  `email:send` list. The mail outbox and the plugin queue are in Postgres
  (`docs/design/mail-queue.md`, `docs/plugin-queue.md`).

## Survey: how comparable hosts run

| Host | Files | Database | Backups and restore | Paging | Source |
|---|---|---|---|---|---|
| WordPress VIP | "VIP File System": an external object store, replicated to other origin data centres and to S3 | One database per environment | Hourly production database backups, kept 30 days; geographic redundancy restores production in another data centre | | https://docs.wpvip.com/infrastructure/ , https://docs.wpvip.com/infrastructure/origin-servers/ |
| Pantheon | Shared filesystem (Valhalla), being replaced in 2026 by object storage behind Cloud Storage FUSE "to make backup and restore faster and more reliable" | Per-site containers, share nothing | Per-site backups | | https://docs.pantheon.io/application-containers , https://docs.pantheon.io/release-notes/2026/08/new-filesystem-launch |
| Upsun (Platform.sh) | Per-project storage in the project's region | Per project | One automated backup a day by default, kept 2 days, retention configurable; backups stay in the project's region | | https://docs.upsun.com/environments/backup.html , https://upsun.com/trust-center/security/data-location/ |
| Ghost(Pro) | Encrypted off-site backups in S3 (2015 write-up; today's stack unverified) | MySQL | | | https://ghost.org/changelog/migrating-to-digitalocean/ |
| Mastodon (self-hosters) | S3 compatible object storage, served from the site's own domain through a proxy | Postgres | | | https://docs.joinmastodon.org/admin/optional/object-storage/ , https://docs.joinmastodon.org/admin/optional/object-storage-proxy/ |
| GitLab.com | Object storage | Postgres | | Pages only on service level indicators burning the error budget fast (2% of the month in 1 hour, 5% in 6 hours); downstream pages suppressed when a dependency is the cause | https://about.gitlab.com/blog/2022/07/19/reducing-pager-fatigue-and-improving-on-call-life , https://runbooks.gitlab.com/monitoring/apdex-alerts-guide/ |
| Google SRE Workbook | | | | Multiwindow, multi burn rate alerts on SLOs: page at burn rate 14.4 (1 h, 5 min short window) and 6 (6 h, 30 min); everything slower is a ticket | https://sre.google/workbook/alerting-on-slos/ |
| AWS Security Reference Architecture | | | | Organization CloudTrail into a separate Log Archive account, bucket writable only by the trail, KMS, versioning and Object Lock | https://docs.aws.amazon.com/prescriptive-guidance/latest/security-reference-architecture/log-archive.html |
| 37signals (leaving the cloud) | Moved 18 PB off S3 to own storage | | | | https://world.hey.com/dhh/our-cloud-exit-savings-will-now-top-ten-million-over-five-years-c7d9b5bd |

What the survey settles, and this design takes:

1. **Files in object storage, served through the application.** Every host that
   scales past one machine keeps uploads in an object store (VIP, Pantheon's
   2026 move, Mastodon, GitLab). Mastodon's proxy pattern, the object store
   behind the site's own domain, is the one that keeps Trovato's access check.
2. **Managed Postgres with point in time recovery and copies elsewhere.** VIP's
   hourly backups and geographic redundancy are the high end; Upsun's one a day
   is the low end. Point in time recovery (5 minute recovery point) plus a daily
   copy to a second region is between them, at managed-service cost.
3. **Page on symptoms, through burn rates.** GitLab and the SRE Workbook agree:
   page on the user-facing error budget burning fast, never on a cause that
   heals itself.
4. **Audit logs in an account production cannot touch.** AWS SRA's log archive.
5. **The 37signals counterpoint** (owning hardware is cheaper at scale) does
   not apply at a few hundred blogs run by one person, where the operating
   burden, not the bill, is the constraint. It is revisited at the growth size.

## Decisions

### 1. Region: Milan (`eu-south-1`), with Frankfurt as the backup region

| | Milan `eu-south-1` | Frankfurt `eu-central-1` |
|---|---|---|
| Latency to Italy | Same country; Milan to Frankfurt alone is about 13 ms (https://www.economize.cloud/resources/aws/latency/eu-south-1-vs-eu-central-1/). No published Italy-to-region figure was found: measure from Italy before applying (README step 1). | One border further |
| Services Trovato needs | EKS, RDS for PostgreSQL, Aurora, RDS Proxy, ElastiCache (Valkey), S3, KMS, Secrets Manager, CloudTrail, NAT all listed (https://api.regional-table.region-services.aws.a2z.com/index.json). **No `m7g` instances** (EC2 or RDS); `m8g` and `t4g` are offered. An opt-in region: enabled per account before use. | Everything, usually first |
| Price | About 1 to 3% under Frankfurt on EC2, RDS and S3; RDS Proxy 33% over ($0.024 against $0.018 per vCPU-hour); NAT $0.050 against $0.052 an hour | Reference |
| Data residency | Italy (EU) | Germany (EU) |

**Milan.** Writers and the operator are in Italy; Italian residency is a plain
answer to Italian bloggers' and the Garante's questions; it is a little cheaper;
and every service this design uses is there (the missing `m7g` is replaced by
`m8g` and `t4g`, and RDS Proxy is not used, decision 4). Readers do not see the
difference either way: Cloudflare serves them from its Italian points of
presence. Frankfurt is the **second region for copies** (database snapshots,
replicated files, the log archive): another EU jurisdiction, so no backup ever
leaves the EU, and far enough away to survive a regional event.

**A second region later (US).** One install per region, never one install across
regions: the same OpenTofu modules applied again with another `envs/` directory
(for example `envs/us-east-2-prod`, its copies in `us-west-2`), its own state
file, its own cluster, database, Redis and buckets, in its own AWS account under
the same organization so a mistake in one region cannot reach the other. A blog
lives in exactly one region; Cloudflare (BG17 custom domains) points each blog's
host names at its region's origin. Moving a blog is export from one install and
import into another (later work). Nothing in Trovato or the modules names
`eu-south-1`: the region, the copy region and every size are variables.

### 2. Compute: EKS with Auto Mode

| | Operations burden (first) | Cost (second) |
|---|---|---|
| **EKS Auto Mode** | AWS runs the control plane, picks and replaces nodes (Karpenter under the hood, https://aws.amazon.com/blogs/containers/announcing-karpenter-1-0), patches node OS images, replaces every node within 21 days, and manages VPC CNI, CoreDNS, kube-proxy, the EBS CSI driver and the load balancer controller. A failed node is replaced with no one awake. The human task left is the Kubernetes minor upgrade, about three a year, in daytime, with 14 months of standard support per version (https://aws.amazon.com/eks/pricing/). | $0.10/h control plane ($73/month) plus a fee of about 12% of each node's EC2 price ($0.00461/h on a `t4g.medium`) |
| EKS managed node groups | As above, but node OS updates, add-on upgrades and node group rolls are ours; Karpenter or the Cluster Autoscaler is ours to run | $73/month, no node fee |
| k3s on EC2 | Ours: control plane availability, etcd backups, certificates, node replacement, OS patching. Every one of those is a night page waiting to happen. | Cheapest: no control plane fee |
| ECS on Fargate | Least: no nodes, no cluster upgrades | About 2.5 times a `t4g.medium` for the same vCPU and memory (Fargate ARM $0.03886 per vCPU-hour and $0.00427 per GB-hour in Milan: 2 vCPU and 4 GB is $0.095 an hour against $0.038); and it discards BG10's Kubernetes manifests, and a deploy or rollback is an AWS API call, which fails the "laptop with cluster credentials alone" requirement (decision 8) |

**EKS Auto Mode.** It is the only option where both night-time failure kinds,
a node dying and a node needing a patch, are handled with no human; its cost
over managed node groups is about $10 a month at launch. Kubernetes keeps BG10's
manifests, probes and drain as they are, and keeps deploy and rollback a
`kubectl` matter. If Auto Mode is not offered in the chosen region when it is
applied, the `eks` module's `auto_mode = false` builds a managed node group of
the same instance types instead (README, step 4); nothing else changes.

- **Node pools.** Auto Mode's `general-purpose` pool, on-demand, `t4g`/`m8g`
  (Graviton: the image is built for `linux/arm64` as well, part 2), spread
  across three availability zones. **No spot at launch.** The web pods stay
  on-demand (a write in flight on a reclaimed node is a writer's lost post),
  and the only other workloads, the migrate Job and the monthly restore drill,
  run minutes a month: a spot pool would save cents and make the drill fail on
  a reclaim, which is a false alarm. Spot is safe, and worth a second pool,
  only for batch work that retries on its own (none yet; a future export or
  reindex job).
- **Autoscaling.** A HorizontalPodAutoscaler on CPU, 2 to 6 replicas at launch,
  a PodDisruptionBudget of `minAvailable: 1`, topology spread across zones;
  Auto Mode adds and removes nodes for the pods. `DATABASE_MAX_CONNECTIONS=10`
  per pod (decision 3 sizes the database for the maximum).

### 3. Database: RDS for PostgreSQL 16, Multi-AZ, no pooler at launch

**RDS for PostgreSQL over Aurora.** Aurora's strengths (failover in about 30
seconds against RDS's 60 to 120, storage that grows itself, cheap read
replicas) are worth their price for a database that needs readers or cannot
pause a minute. trovato.blog's readers are served by Cloudflare; a minute of
failover delays writes, which retry. RDS Multi-AZ is one standby billed with
the instance ($0.145/h for `db.t4g.medium` in Milan); Aurora's equivalent is
two instances ($0.081/h each) plus I/O charges or the I/O-Optimized premium.
RDS is also plain Postgres: `pg_dump`, `pg_restore` and the restore drill work
the same against RDS, the CI database and a self hoster's server. Aurora stays
the named upgrade at the growth size if failover time ever matters more than
it does now.

- **Version 16**, the version CI tests on (`.github/workflows/ci.yml`), so
  production never runs a Postgres the tests have not.
- **Multi-AZ** (one synchronous standby, automatic failover). A failover heals
  itself: it is a morning item, not a page.
- **Sizes.** Launch: `db.t4g.medium` (2 vCPU, 4 GiB), 50 GB `gp3` with storage
  autoscaling to 200 GB. Growth: `db.m8g.xlarge` (4 vCPU, 16 GiB), 500 GB.
- **Point in time recovery.** Automated backups kept 14 days (recovery point
  about 5 minutes); daily snapshots by AWS Backup, copied to Frankfurt
  (decision 10).
- **Connection pooling: none at launch, PgBouncer when needed, never RDS Proxy.**
  Six pods at most times ten connections is 60, plus the migrate Job's few;
  `db.t4g.medium`'s `max_connections` default is about 400 (RDS derives it
  from memory), and `db.m8g.xlarge`'s about 1,700. A pooler adds a moving part
  for no gain until pods times pool size passes about 60% of
  `max_connections`: about 25 pods at launch size, about 100 at growth size,
  more than either needs. If that is ever reached, the answer is PgBouncer 1.21 or later in transaction mode with
  `max_prepared_statements` set, as a two-replica Deployment: sqlx prepares
  statements, which pins every RDS Proxy connection to one client and removes
  the multiplexing it is paid for. `trovato migrate` connects to RDS directly
  either way, because its lock is a session-level `pg_advisory_lock`.
- **Parameters.** A parameter group with `rds.force_ssl = 1`,
  `log_min_duration_statement = 1000`, `idle_in_transaction_session_timeout =
  60000`, `statement_timeout = 0` (Trovato sets its own per statement) and
  `shared_preload_libraries` left at the default (`pg_stat_statements`).
  `pgvector` is available on RDS 16; the migrate Job runs as the database owner
  so `CREATE EXTENSION vector` succeeds, while the pods run as a role that owns
  nothing and can only read and write.

### 4. Redis: ElastiCache for Valkey, primary and replica with failover

**Valkey**, the open source continuation of Redis that ElastiCache prices 20%
under Redis OSS ($0.0288/h against $0.036/h for `cache.t4g.small`), wire
compatible with the `redis` and `fred` clients Trovato uses. Launch: one
`cache.t4g.small` primary and one replica in another zone, automatic failover,
in-transit encryption and an AUTH token. Growth: `cache.t4g.medium`.

What Trovato loses if Redis restarts empty, and why that is acceptable:

| Kept in Redis | Lost on an empty restart | Acceptable because |
|---|---|---|
| Sessions | Everyone signed in is signed out | Readers never sign in; writers sign in again. A failover to the replica keeps them; only losing both nodes empties it. |
| Rate limit counters | Reset to zero | A minute's limits restart; no harm. |
| Shared cache and `cache:gen` | Cold cache; every server sees the generations change and clears its own caches | Postgres answers until the cache refills; Cloudflare still serves readers. |
| Cron locks | Released | The next run takes the lock; cron work is idempotent by design (BG10, B178). |
| Legacy `email:send` list | Mail not yet sent from it | New mail goes through the Postgres outbox (`docs/design/mail-queue.md`), which is not in Redis. |

So Redis is rebuildable: no backup is restored into it (decision 10), and its
loss is a morning item unless writes fail with it.

### 5. Files: an S3 `FileStorage` (built in part 1)

`S3FileStorage` (`crates/kernel/src/file/storage.rs`) implements the same trait
as `LocalFileStorage`, for any S3 compatible store: AWS S3, MinIO (CI and local),
and the S3 APIs other providers offer to self hosters. Chosen by
`FILE_STORAGE=s3` with `S3_BUCKET`, `S3_REGION`, optionally `S3_ENDPOINT`,
`S3_PREFIX` and `S3_ALLOW_HTTP`; `FILE_STORAGE` unset or `local` keeps the
directory, so single server and self hosted installs work exactly as before.

- **URIs do not change.** Every backend stores a file under the URI it always
  had, `local://<key>`: the scheme names the site's file store, not the medium.
  No `file_managed` row, file field or item body is rewritten when a site moves
  to a bucket, and the move can be repeated or undone.
- **Served through Trovato, never from the bucket.** `public_url` stays
  `FILES_URL/<key>`. Every request reaches `/files/`, which checks access first
  and sends the `public, max-age=604800` header only for what an anonymous
  visitor may fetch; Cloudflare keeps a copy of exactly those. Private files are
  served only after the access check, `private, no-store`. No signed URLs: a
  signed URL is a bearer token a reader can forward, valid until it expires, and
  would move the access check from Trovato to whoever holds the link. The
  bucket is **never public**: Block Public Access on, ACLs disabled (bucket
  owner enforced), and a bucket policy that denies any request without TLS
  (`aws:SecureTransport = false`) and denies every principal whose
  `aws:PrincipalArn` is not on a named list: the web role, the replication
  role, the drill role and the break-glass admin role. The bucket (or prefix)
  holds only Trovato's files: an object with no `file_managed` row is served as
  an unmanaged file to anyone the route allows, so nothing else may be put
  there; in a bucket shared by several sites every site has its own
  `S3_PREFIX` and no role is ever granted the whole bucket. A derivative is
  served only while its original is stored, so a deleted private image's
  leftover derivatives never become public.
- **Image styles in S3 too.** Derivatives are read and written through the trait
  at `local://styles/<style>/<path>` (`services::image_style::derivative_uri`),
  so one server makes a derivative and every server serves it. With a directory
  that is the same file the disk cache always used. The `/files/` route still
  never serves the `styles/` tree; only the image style route does, after
  checking access against the original.
- **Not found is a type.** Both backends return `StorageNotFound` for a missing
  file, and `is_not_found` recognises it (and an I/O `NotFound`), replacing the
  text match that never matched.
- **Local writes are atomic.** `LocalFileStorage::write` writes a temporary file
  beside the target and renames it, so a reader on any server sees the old
  bytes or the new, as S3 guarantees. Its key check also refuses `.`, empty
  components and a leading `/` (which `Path::join` treats as absolute), as the
  S3 backend does.
- **Startup check for both.** `FileStorage::check_usable` writes, reads back
  and deletes a uniquely named object; a server refuses to start, naming the
  bucket or directory, when it fails, and the status report runs it too.
- **Migration: `trovato files migrate-to-s3`.** Copies every regular file under
  `UPLOADS_DIR` (or `--from`) into the configured bucket under the same key,
  `--parallel` at a time. Each copy is read back and compared by SHA-256; a file
  the bucket already holds with the same SHA-256 is skipped, so an interrupted
  or partly failed run is finished by running it again. Symbolic links are not
  followed (one could name a file outside the directory), and Trovato's own
  sentinel and temporary files are skipped. Nothing is deleted on either side:
  the directory stays as the way back. Exits nonzero, listing each file, if
  any was not copied.
- **Credentials are never Trovato settings.** The client reads the standard AWS
  sources: EKS Pod Identity (`AWS_CONTAINER_CREDENTIALS_FULL_URI`), IRSA web
  identity, the instance role, or `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` for
  a self hosted store. On trovato.blog there is no static key: the pod's role
  is short lived and rotated by AWS. The client's `Debug` prints only the bucket
  and prefix, and errors name the bucket, never a credential.
- **Failures are reported quickly:** 5 seconds to connect, 30 to finish, and a
  failed request retried at most three times while less than ten seconds have
  passed (so a hung request is one 30 second attempt). A request is waiting; a
  store down for longer is better reported than hidden. A server that cannot
  reach the store at startup exits and Kubernetes restarts it with backoff, so
  it heals by itself when the store returns: a morning item, and a page only
  through the writes-failing rule if every pod is down.
- **Style names are machine names.** The image style route accepts only
  `[A-Za-z0-9_-]` for the style and holds the original's path to the `/files/`
  route's own rules. A style of `w400%2F2026%2F11` (axum decodes `%2F`) or an
  original of `styles/w400/...` would otherwise reach a private image's real
  derivative while the access check ran on a path naming no file. This was
  already reachable on `main` through the disk cache; part 1 closes it.
- **Trovato's working names are never served:** `/files/` refuses
  `.trovato-write-check-*` and a local write's `.<name>.trovato-tmp-*`.

**The crate: `object_store` 0.14** (Apache Arrow project; used by Arrow, Delta
Lake and InfluxDB). Nothing in the workspace speaks S3, and the standard set has
no S3 client. Built with `default-features = false` and
`aws-base, reqwest, ring`: `ring` is already in the tree (the default would add
`aws-lc-rs` and a C build), and TLS comes from `reqwest` 0.13 with native TLS,
which the kernel already enables for OTLP export. It adds a handful of small
crates (`quick-xml`, `crc-fast`, `md-5`, `humantime`), against about a hundred
for the AWS SDK (`aws-sdk-s3` with `aws-config`, which brings its own HTTP and
TLS stack). Writing a SigV4 client by hand was rejected: signing is small, but
the credential providers (Pod Identity, IRSA, container and instance roles,
their refresh) are security code better taken from a maintained library.

**What still needs a shared directory.** The Pagefind index
(`PAGEFIND_OUTPUT_DIR`) is written by the server whose cron run takes the lock
and served from disk by every server. It is derived data, rebuilt from Postgres,
a few megabytes per site. At launch it stays on a small EFS volume (managed,
multi-AZ, `ReadWriteMany`, through the EKS EFS CSI add-on; about $0.33 per GB
a month); moving it into the file store is a named follow-up, not part of
BG16. `file` import records (`import/file.rs`) still need files in a local
directory; blog moves between regions (later work) will import into the
store.

### 6. Secrets: Secrets Manager through External Secrets, Pod Identity per service

- **Where.** AWS Secrets Manager, one secret per concern ($0.40 a month each):
  `trovato/db-app` (the app role's password, the URL built from it),
  `trovato/db-owner` (the migrate Job's role; the RDS master password is
  managed by RDS itself, `manage_master_user_password`), `trovato/redis`
  (AUTH token), `trovato/stripe`, `trovato/mail`, `trovato/cloudflare`, and
  `trovato/app-keys` (session, signing and `METRICS_TOKEN`/`HEALTH_TOKEN`).
  Nothing secret is in the repository, an image, a tofu variable file or the
  state: tofu creates each secret empty, and values are written only by the
  rotation script (state holds the secret's name, never its value).
- **How pods get them.** External Secrets Operator (CNCF; Helm chart pinned by
  version) syncs each into a Kubernetes Secret the Deployment reads, and
  refreshes it hourly. Chosen over the Secrets Store CSI driver because a
  rotated value reaches pods as an ordinary Secret change, which a rollout
  restart picks up, with no volume mounts to reason about.
- **Least privilege per service (EKS Pod Identity).** `trovato-web`:
  `s3:GetObject`, `s3:PutObject` and `s3:DeleteObject` on
  `arn:aws:s3:::<files-bucket>/<S3_PREFIX>/*`, `s3:ListBucket` on the bucket
  with the condition `s3:prefix` like `<S3_PREFIX>/*` (without it S3 answers a
  missing key with 403 rather than 404, which Trovato would report as a store
  failure), `kms:GenerateDataKey` and `kms:Decrypt` on the bucket's key only,
  and `secretsmanager:GetSecretValue` on its own secrets; no `s3:*`, no bucket
  policy or ACL action. `trovato files migrate-to-s3` runs as a one-off Job
  under the same role. Two sites sharing a bucket get one role each, scoped to
  their own prefix. `trovato-migrate`: read `db-owner` only. `external-secrets`: read `trovato/*` only. The drill
  role: restore into and delete `drill-*` instances only. No pod has any
  permission over IAM, KMS key policy, backups or logs.
- **The provider can be breached.** No long lived key anywhere: pods use Pod
  Identity; CI reaches AWS through GitHub OIDC (`token.actions.githubusercontent.com`)
  assuming a role whose trust policy names this repository and the `main`
  branch or a `production` environment, for one hour; people use IAM Identity
  Center, which issues short lived sessions. A CI check (part 2) fails any
  workflow file that contains an AWS access key ID (`AKIA`/`ASIA` followed by
  16 characters) or passes `aws-access-key-id` to an action. The CI test MinIO
  uses credentials made at random per run by `scripts/ci-start-minio.sh`, never
  written into a workflow.
- **Rotation in one command with no downtime** (`deploy/aws/scripts/rotate-secrets.sh`,
  part 3), every secret in turn:
  - database: two app roles (`trovato_app_a`, `trovato_app_b`); the one not in
    use gets a new password, the secret switches to it, pods roll one at a
    time, then the old role's password is changed. Connections already open
    keep working throughout (the alternating users pattern AWS documents for
    Secrets Manager rotation);
  - Redis: ElastiCache's `ROTATE` strategy accepts the old and new AUTH tokens
    together until the pods have rolled, then `SET` retires the old one;
  - Stripe, mail, Cloudflare: a new key from the provider's API (Stripe's
    rolled key keeps the old one valid for a set time; SES SMTP credentials
    are an IAM key pair rotated by creating the second before deleting the
    first; Cloudflare tokens are rolled by API), stored, pods rolled, old key
    revoked;
  - session and signing keys: a new key is added beside the old, pods roll,
    and the old is removed after the session lifetime.
  The drill (part 3) runs it end to end against a local Postgres, Valkey and
  MinIO while a load generator sends requests, and counts failed requests:
  the pass mark is zero.

### 7. Audit trail: a log account production cannot touch

- **AWS Organizations** with: a management account (billing, Identity Center,
  nothing else), a `log-archive` account, and one account per region install
  (`prod-eu`, later `prod-us`).
- **CloudTrail organization trail**, all regions, management events plus S3
  data events for the files and backup buckets, delivered to a bucket in
  `log-archive` (Frankfurt): versioned, KMS encrypted with a key owned by that
  account, **S3 Object Lock in compliance mode, 400 days** (at least a year,
  plus room), a bucket policy that allows only the trail to write and denies
  every delete. No role in a production account has any permission in
  `log-archive`: a breach of production cannot erase its own tracks
  (AWS SRA, Well-Architected FND05-BP03).
- **GitHub audit log, git events included.** GitHub keeps git events (clone,
  fetch, push) only 7 days (https://docs.github.com/en/enterprise-cloud@latest/admin/monitoring-activity-in-your-enterprise/reviewing-audit-logs-for-your-enterprise/using-the-audit-log-api-for-your-enterprise),
  so they are **streamed**, not fetched later, to an S3 bucket in `log-archive`
  with the same Object Lock, GitHub authenticating with OIDC (no stored key).
  Audit log streaming is an Enterprise Cloud feature: the repository moves to
  an organization on GitHub Enterprise Cloud ($21 per user a month, two users),
  a cost named in the table below and a step for Jeremy.
- **Alerts on unusual IAM activity** (page now, decision 11): EventBridge rules
  on the trail for root sign-in, `CreateUser`, `CreateAccessKey`,
  `CreateLoginProfile`, any `Attach*Policy`/`Put*Policy`/`CreatePolicyVersion`,
  and changes to the trail, the log bucket or a backup vault; and GuardDuty for
  access from unusual networks and anomalous API use (findings of high
  severity page; medium are morning items).

### 8. Deploy and recovery without a vendor console

- **Deploy and rollback are `kubectl` only.** `deploy/aws/scripts/deploy.sh
  <image digest>` runs the migrate Job and then sets the Deployment's image;
  `rollback.sh` redeploys the previous image (`kubectl rollout undo`, or a
  named digest), as BG10's upgrade procedure does. Neither calls the AWS CLI,
  the console or any provider API: they need a kubeconfig and nothing else.
- **Break glass.** Each of **two named admins** (Jeremy and a second person
  Jeremy names, README step 2) holds:
  1. an IAM Identity Center user with a hardware security key, for the normal
     path;
  2. a break-glass kubeconfig for a `break-glass` Kubernetes service account
     bound to deploy and rollback rights in the `trovato` namespace only, with a
     token valid 30 days, re-issued monthly by the rotation script and kept in
     the admin's password manager. Kubernetes validates it itself, so it works
     when Identity Center, STS or the console do not;
  3. the root user of each account: password in the password manager, a
     hardware security key, no access key (root access keys are refused by an
     organization policy), use paged (decision 7).
  The break-glass path is exercised by the quarterly drill in the on call
  guide, by each admin, so recovery never waits on one person.

### 9. Build supply chain

- **Pinned base images.** `Dockerfile` `FROM` lines name a digest
  (`rust:1-bookworm@sha256:...`, `debian:bookworm-slim@sha256:...`), updated by
  a pull request, never silently. Part 2 makes the change and adds a CI check
  that refuses an unpinned `FROM`.
- **Mirrored inputs.** ECR pull-through cache rules for Docker Hub and GHCR, so
  base images come from the account's own registry. Rust crates: each release
  build also pushes a `trovato-builder:<Cargo.lock hash>` image with the
  dependency sources fetched (`cargo fetch`, the cargo-chef layer the
  Dockerfile already has); an emergency build uses it with `--offline`, so a
  crates.io or GitHub outage cannot block a fix.
- **Known good images kept.** ECR lifecycle: keep the last 30 release images
  and every image tagged `known-good-*` (the deploy script tags an image
  `known-good-<date>` after it has served an hour without a page), expire
  untagged images after 14 days. Images are referenced by digest, tags are
  immutable.

### 10. Backups and restore

| What | How | Recovery point | Recovery time |
|---|---|---|---|
| Database, an error or bad migration | RDS point in time restore to a new instance, then switch the Secret | 5 minutes | 30 to 60 minutes at launch size (measured by the drill) |
| Database, region lost | AWS Backup daily snapshot, copied to a Frankfurt vault with **Vault Lock in compliance mode** (35 days), so a compromised production account cannot delete it | 24 hours | 4 hours: `tofu apply` of the same modules in Frankfurt, restore, DNS |
| Files | S3 versioning (noncurrent versions kept 90 days) and replication to a Frankfurt bucket, itself versioned | Minutes (replication) | Minutes: point the pods at the replica, or copy back |
| Redis | Not backed up: rebuildable (decision 4) | | Minutes (a new node) |
| Infrastructure | The repository and the tofu state (versioned bucket) | | `tofu apply` |

- **The restore runbook** (`docs/deployment/restore-runbook.md`, part 3) gives
  each row's literal commands.
- **The restore drill** (`deploy/aws/scripts/restore-drill.sh`, part 3)
  restores the latest recovery point into a scratch environment (a `drill-*`
  RDS instance and a `drill-*` bucket prefix, on the on-demand pool), runs
  `trovato migrate --status` and smoke tests (`/health/ready`, a known item
  page, a known private file refused anonymously and served to an admin),
  prints the measured restore time, and deletes the scratch environment. It is
  scheduled monthly (a Kubernetes CronJob), and the same
  script runs end to end against a local Postgres and MinIO in CI.

### 11. Alerting: SLOs, and who gets woken

**SLOs** (30 day windows):

| SLO | Target | Measured from |
|---|---|---|
| Writes and admin available | 99.5% of `POST`/`PUT`/`DELETE` and `/admin` requests answer without a 5xx | `http_requests` by method, route and status (BG14) |
| Writes and admin latency | 95% under 1 second | `http_request_duration_seconds` |
| Reads served from cache | 90% of reader page views are Cloudflare cache hits | Cloudflare analytics (a dashboard, not a page: a low hit rate costs money, not readers) |

**Page now** (Pushover emergency priority: repeats until acknowledged):

1. Writes failing: the availability SLO burning at 14.4 times over an hour and
   five minutes (2% of the month's budget in an hour), or the external check
   of `/health/ready` failing from two locations for 10 minutes, checked both
   through Cloudflare and at the origin directly: a failure through Cloudflare
   alone is Cloudflare's outage, which no one here can fix, and is a morning
   item with a status page note. This is the "all writes failing for more
   than N minutes" rule, with N = 10.
2. Data loss risk: RDS instance not available for 10 minutes (a failover that
   did not heal), database free storage under 5% with autoscaling at its
   maximum. (Uploads failing count toward rule 1: they are writes.)
3. Backups failing: no new AWS Backup recovery point in 48 hours (two missed
   days, not one late job), or point in time recovery disabled.
4. Certificate or DNS: trovato.blog not resolving, or its TLS handshake
   failing, from the external checker for 10 minutes.
5. Security: root sign-in, a new IAM user or access key, a policy change by
   any principal other than the CI apply role and Identity Center's own
   provisioning (their changes are expected, and are morning items), a
   change to CloudTrail, the log bucket or a backup vault, a high severity
   GuardDuty finding.

**Look in the morning** (email and the Grafana list, never a push): a pod
restarted or evicted, a node replaced, an RDS or Valkey failover that
completed, Redis restarted empty, the autoscaler at its maximum, the latency
SLO or a slow availability burn (6 times over 6 hours), Cloudflare hit rate
under target, storage over 80%, a certificate expiring within 21 days (and
again at 7), a failed backup copy job to Frankfurt (the next day's retries
it; point in time recovery covers the gap), S3 replication failures, a failed
monthly restore drill, a medium GuardDuty finding, an image or dependency
advisory. Everything here
heals itself or can wait.

**Where.** Grafana Cloud's free tier (10,000 metric series, alerting, 3 IRM
users, 100,000 synthetic checks a month; $19 a month Pro if outgrown,
https://grafana.com/pricing/) holds the rules and runs the external checks,
**outside AWS**, so an AWS outage still pages. Grafana Alloy in the cluster
scrapes `/metrics` with `METRICS_TOKEN` and forwards CloudWatch metrics
(RDS, ElastiCache, AWS Backup, S3 replication) through Grafana Cloud's
CloudWatch integration with a read-only role; EventBridge security events go
to an SNS topic that Grafana Cloud's alerting receives. Rules are code
(`deploy/aws/observability/`, part 3). **Phone push: Pushover**, $4.99 one time
per platform per admin (https://pushover.net/pricing), with emergency priority
(repeats every 60 seconds until acknowledged, overrides quiet hours). Chosen
over PagerDuty's free tier (100 SMS a month, more to set up for two people)
and ntfy (best effort, a single server). Its servers are in the US, outside
AWS's EU regions and Cloudflare.

### 12. Status page: Upptime on GitHub Pages

**Upptime** (https://github.com/upptime/upptime): a separate public repository
whose GitHub Actions check trovato.blog, a sample blog, the admin sign-in page
and `/health/ready` every 5 minutes, records incidents as GitHub issues, and
publishes a static status page on GitHub Pages. **$0.** It runs on GitHub's
infrastructure (Azure and GitHub's Pages CDN), so it stays up when AWS or
Cloudflare's proxy is down. It is published at
`https://trovato-status.github.io/` (or the organization's Pages address) and
at `status.trovato.blog` through a **DNS-only** (not proxied) Cloudflare record;
the GitHub Pages address is the one printed in the on call guide and the
customer notice, for the day Cloudflare's DNS itself is the problem. Chosen
over Better Stack (its status pages run on AWS) and Instatus or UptimeRobot
(hosting not stated) because it is free, lives in git, and its hosting is known
to be outside both.

### 13. Infrastructure as code: OpenTofu under `deploy/aws/`

- **OpenTofu** (MPL-2.0, a CNCF project, https://www.cncf.io/projects/opentofu/),
  not Terraform (BUSL since 1.6). OpenTofu's state encryption (1.7+,
  https://opentofu.org/docs/v1.7/language/state/encryption/) encrypts state and
  plans with an AWS KMS key, so even a leaked state file holds nothing
  readable. Remote state in an S3 bucket (versioned, KMS, Block Public Access)
  with S3 native locking (`use_lockfile = true`), no DynamoDB table.
- **Layout** (part 2): `deploy/aws/bootstrap/` (the state bucket and KMS key,
  applied once per account with local state, then migrated into the bucket),
  `deploy/aws/modules/{network,eks,database,cache,storage,secrets,backup,audit,
  registry,github-oidc,observability}`, `deploy/aws/envs/log-archive/`,
  `deploy/aws/envs/eu-south-1-prod/`, and `deploy/aws/envs/scratch/` for a
  throwaway copy whose `tofu destroy` is documented. One `tofu apply` per
  environment from a clean account.
- **Checks in CI** (part 2): `tofu fmt -check -recursive`, `tofu validate` per
  environment (with `-backend=false`), `tflint` with the AWS ruleset, and
  `trivy config` with zero findings of high or critical severity, any accepted
  lower finding named with its reason in the module.

## Monthly cost

USD, on-demand, 730 hours a month, Milan prices from the AWS price list
(offer files of 2026-11-01). Rounded; "est." marks a figure from a size not
in the price list (doubled from the next size down) or a usage guess.

| Item | Launch (a few hundred blogs) | Growth (ten thousand blogs) | Each extra region (launch size) |
|---|---|---|---|
| EKS control plane | 73 | 73 | 73 |
| Nodes (Auto Mode fee included) | 3 × `t4g.medium`: 94 | 4 × `m8g.large`: 343 | 94 |
| RDS PostgreSQL Multi-AZ | `db.t4g.medium` 106, 50 GB gp3 13: 119 | `db.m8g.xlarge` 575 (est.), 500 GB 133: 708 | 119 |
| Connection pooler | none | none (about 100 pods before one is needed) | none |
| Valkey primary + replica | 2 × `cache.t4g.small`: 42 | 2 × `cache.t4g.medium`: 84 (est.) | 42 |
| NAT gateway (one) and data | 39 | 62 | 39 |
| Application load balancer | 28 | 80 | 28 |
| EFS (Pagefind index) | 1 | 10 (est.) | 1 |
| S3 files + Frankfurt replica + requests | 5 | 120 | 5 |
| AWS Backup snapshots + Frankfurt copies | 10 | 105 | 10 |
| Secrets Manager (7) and KMS (4) | 7 | 7 | 7 |
| CloudTrail data events, GuardDuty, CloudWatch | 17 (est.) | 80 (est.) | 15 (est.) |
| Log archive bucket (Object Lock) | 1 | 5 | shared |
| ECR | 1 | 1 | 1 |
| Internet egress to Cloudflare | 5 (est.) | 92 (est.) | 5 (est.) |
| Grafana Cloud | 0 (free tier) | 50 (Pro, est.) | shared |
| GitHub Enterprise Cloud (2 users) | 42 | 42 | shared |
| Pushover | about 10 once (2 admins) | | shared |
| Upptime status page | 0 | 0 | shared |
| **Total a month** | **about 485** | **about 1,860** | **about 440** |

Cloudflare (BG9, BG17) is outside this table. At the growth size the database
is two fifths of the bill; Aurora I/O-Optimized or reserved instances (about 30
to 40% off for a one year commitment) are the levers, and the 37signals
argument is worth rechecking then.

## Security review of this design

- No security check in Trovato is weakened. The access check on every file is
  kept by design; nothing hands out a URL into the store; the bucket is never
  public.
- The only new trust is the S3 client crate, a maintained Apache project, and
  the credential sources it reads, which are AWS's standard chain.
- A breach of the production account cannot delete its audit trail (Object
  Lock in another account), its backups (Vault Lock in compliance mode) or
  hide new keys (paged). A breach of the repository or CI cannot reach AWS
  beyond a one-hour role limited to deploying images.

## What this does not build, and why

- **The AWS accounts, purchases and the first apply**: Jeremy's, with the
  README (part 2).
- **A second region**: designed for (decision 1), not built.
- **Moving blogs between regions**: later work.
- **The Pagefind index in the file store**: a follow-up; launch uses EFS.
- **Signed URLs**: rejected (decision 5).
- **RDS Proxy and Aurora**: rejected for now (decision 3), named upgrades.

## Parts

1. `bg16-s3-storage`: this document; `S3FileStorage`, `FILE_STORAGE` and the
   `S3_*` settings; derivatives through the store; `trovato files
   migrate-to-s3`; the storage conformance suite and the S3 serving tests
   against MinIO in CI.
2. `bg16-infra`: `deploy/aws/` (bootstrap, modules, environments), the CI
   checks (`tofu fmt`, `tofu validate`, `tflint`, `trivy config`, no static
   AWS key in any workflow, pinned `FROM` digests), Kubernetes manifests for
   EKS (External Secrets, Pod Identity, HPA, PDB), `deploy/aws/README.md`.
3. `bg16-ops`: the restore drill and rotation drill scripts and their local CI
   runs, alert rules, the Upptime configuration, the restore runbook, the on
   call guide and the breach runbook.

## Tests (part 1)

- `crates/kernel/tests/file_storage_conformance_test.rs`: one contract
  (`conformance`) run against `LocalFileStorage` and `S3FileStorage`: round trip
  of awkward bytes, empty files, replace, not found as a type, delete twice,
  every traversal spelling refused (not reported missing), the startup check;
  bucket prefixes keep sites apart; a missing bucket is refused by name with no
  credential in the message; the migration copies every file once with
  checksums, skips sentinels, temporary files and symbolic links, and resumes
  (a copied file kept, a stale copy replaced, a bad name reported without
  stopping the rest), against a directory and against S3.
- `crates/kernel/tests/file_storage_s3_serving_test.rs`: a whole app on
  `FILE_STORAGE=s3`: an upload lands in the bucket and not on disk, and a
  second client reads it; a private file is 404 to anonymous and served
  `private, no-store` to an admin; a public file is served `public,
  max-age=604800` with its type and `nosniff`; an unsigned request to the
  bucket for a public file's object is refused; a derivative is stored in the
  bucket, served from it, and a restricted image's derivative is still 404 to
  anonymous.
- The conformance file also proves the migration refuses a copy whose read-back
  differs (a store that keeps other bytes than it was given), and that a
  symbolic link to a directory outside is not walked into.
- `crates/kernel/tests/file_storage_s3_serving_test.rs` also proves a private
  derivative is not reached through a `%2F` style name or an original under
  `styles/`, and that Trovato's working objects are never served.
- `crates/kernel/tests/zz_file_store_cli_test.rs`: the real binary. `serve`
  with an unusable bucket exits nonzero naming it, before any database (none
  listens at the address it is given); `files migrate-to-s3` refuses without an
  S3 destination, prints its one line, and exits nonzero naming a file it
  could not copy.
- Unit tests: style names are machine names (`routes/image_style.rs`); the
  `/files/` path rule refuses Trovato's working names (`routes/file.rs`).
- `config.rs` unit tests: `FILE_STORAGE` defaults to local, reads the `S3_*`
  settings, and refuses `s3` without a bucket, an unknown store and a bad
  `S3_ALLOW_HTTP`.
- The S3 tests run against MinIO in CI (`scripts/ci-start-minio.sh`, in the
  Test and Coverage jobs) and fail, rather than skip, when it is missing there.
