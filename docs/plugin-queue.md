# Plugin Queue (v2)

The plugin queue lets a plugin enqueue work — typically from `tap_cron` — for
the kernel to process later by calling `tap_queue_worker` on the owning plugin.
Version 2 (P11d / decisions D-45..D-48) makes it a real work queue: attempts,
exponential backoff, priority, claim-locking, and a dead-letter tier.

This document is the contract for plugin authors and operators.

## Enqueuing work

Two host functions enqueue jobs. Both inject the calling plugin's name (a plugin
cannot enqueue under another plugin's identity) and feed the same server-side v2
semantics.

- **`queue_push(queue_name, payload)`** — the minimal, byte-identical form.
  Priority 0, no delay. Its ABI is frozen; existing callers are unaffected.

- **`queue_enqueue(queue_name, payload, opts)`** — the additive form (D-48). `opts`
  is a `QueueOptions`:

  ```rust
  use trovato_sdk::host::queue_enqueue;
  use trovato_sdk::types::QueueOptions;

  queue_enqueue(
      "my_import",
      &serde_json::json!({ "batch": 7 }),
      &QueueOptions { priority: 10, delay: 0 },  // higher priority drains first
  )?;
  ```

  - `priority` (i32, default 0): higher values are claimed first; ties break by
    insertion order (FIFO).
  - `delay` (seconds, default 0): defer the first attempt by this many seconds.

`payload` must be well-formed JSON. Error codes are documented in
[`plugin-error-codes.md`](plugin-error-codes.md#queue-api-errors).

## Declaring your worker and its concurrency

Implement two taps:

- `tap_queue_info` — declare the queue(s) you own and their worker concurrency:

  ```rust
  #[plugin_tap]
  fn tap_queue_info() -> serde_json::Value {
      serde_json::json!([{ "name": "my_import", "concurrency": 4 }])
  }
  ```

- `tap_queue_worker` — process one job. The kernel passes the job payload
  **directly** as the worker input (not wrapped):

  ```rust
  #[plugin_tap]
  fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
      // ... do the work; return anything on success ...
      serde_json::json!({ "status": "ok" })
  }
  ```

**Concurrency (D-47).** `concurrency` is now honored — for the first time; in v1
it was only a codegen docstring and was never parsed. The kernel drains up to
that many jobs **in parallel** per plugin, **clamped to a kernel cap of 4**. A
plugin that declares nothing (or exports no `tap_queue_info`) drains at
concurrency 1.

## Delivery guarantee: at-least-once — workers MUST be idempotent

> **The queue delivers each job _at least once_.** A job whose worker is
> mid-flight when its claimer crashes keeps its incremented attempt count and is
> retried once its claim lease expires. **Write idempotent workers.**

This is exactly the duplicate-row class the reference importer hit and had to
defend against — see `ritrovo_importer/migrations/002_dedup_conferences.sql`,
which deduplicates conferences by `field_source_id` precisely because a job may
be delivered more than once. Use upserts / natural keys / dedup guards; never
assume a job body runs exactly once.

### What counts as success vs. failure

- **Success**: `tap_queue_worker` returns (any value). The job is deleted.
  Returning an error-shaped body such as `{"status":"error"}` is still a
  *successful dispatch* — the job is consumed, not retried. (This matches how
  the reference importer's error returns behave.)
- **Failure**: the worker **traps** (panics, exhausts its epoch/fuel budget, or
  returns an error-length result). The kernel records a failed attempt.

To make a job retry, fail loudly (trap); to consume it, return.

## Retry, backoff, and dead-lettering

On a failed attempt the job is rescheduled with **exponential backoff**
(`60s * 2^(attempts-1)`, capped at 1 hour). Once `attempts` reaches
`max_attempts` (default 5) the job is moved to the **dead-letter** tier
(`status = 'dead'`) with its last error preserved. Consequences:

- **Nothing retries forever.** A permanently failing ("poison") job dead-letters
  instead of blocking its queue — the head-of-line stall of v1 is gone.
- **Fairness.** At most 100 jobs per plugin are processed per drain cycle, so one
  plugin flooding its queue cannot starve another.

## Cadence: when does the queue drain?

Two execution modes (the ratified hybrid, D-46). The default is unchanged from
v1.

- **External cron (default).** The queue is drained inside the cron request when
  something hits `POST /cron/{key}`. **The latency floor is whatever your
  external trigger's cadence is** (e.g. a systemd timer or crond hitting the
  endpoint every minute). No in-process scheduler runs. This is the default and
  the minimal deployment's contract.

- **Resident queue-runner (opt-in).** For sub-cron latency, enable a
  kernel-owned background task that drains on its own cadence. When enabled, the
  cron-request drain **steps aside** (no double-draining); the runner owns
  draining. Claims use `FOR UPDATE SKIP LOCKED`, so even overlapping drainers
  never double-deliver beyond the at-least-once contract.

### Enabling the resident runner (operators)

Set the `queue_runner` key in `site_config` (default tenant):

```sql
-- Enable with a 5-second poll interval:
INSERT INTO site_config (key, value, updated)
VALUES ('queue_runner', '{"enabled": true, "poll_interval_secs": 5}'::jsonb, NOW())
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated = NOW();
```

- `enabled` (bool, default `false`)
- `poll_interval_secs` (u64, default 5; floored at 1)

**Runtime behavior:** the runner is **spawned at boot only when `enabled` is true**
— the default deployment starts no scheduler. The runner and the cron drain both
re-read this key each cycle, so a **runtime disable** takes effect without a
restart (the runner idles, cron resumes draining). **Enabling** at runtime
requires a restart to spawn the task.

**Deployment note (ops runbook).** The resident runner needs a long-lived server
process; it participates in graceful shutdown (it drains-in-progress jobs finish,
then the loop exits on the shutdown signal). No new unit is required:

- **systemd:** the existing `trovato` service already runs the long-lived
  process; `SIGTERM` triggers graceful shutdown (bounded by `SHUTDOWN_TIMEOUT_SECS`).
  If the runner is enabled you can drop the external cron timer that pokes
  `/cron/{key}` (the runner owns draining), or keep it as a harmless fallback.
- **docker/compose:** the app container is the runner host; ensure `stop_grace_period`
  is at least `SHUTDOWN_TIMEOUT_SECS` so in-flight jobs finish on shutdown.

## Kernel-internal embed queue (P11f / D-51, D-52)

The kernel is itself a queue producer/consumer. Item save enqueues a **kernel
embed job** — under the reserved `plugin_name` `__kernel_embed` (a leading
underscore can never be a plugin machine name, so it never collides), queue name
`embed` — instead of embedding inline on the save path. A **native drain arm**
(`CronService::drain_embed_jobs`) claims and runs those rows: unlike a plugin
job it dispatches no `tap_queue_worker`, it embeds the item natively. It shares
the **same** `plugin_queue` table, the same `FOR UPDATE SKIP LOCKED` claim, the
same per-cycle fairness cap, and the same retry/backoff/dead-letter bookkeeping
as plugin jobs — embed jobs are first-class queue-v2 citizens. Its worker width
is fixed at the kernel concurrency ceiling (4); it has no `tap_queue_info`.

It drains on the **same cadence** as plugin queues (external cron by default;
the resident runner when enabled, above) and follows the same runner hand-off.

Enqueue is decoupled from live pgvector availability: the durable "this item
needs embedding" intent is recorded at save time; the drain decides what to do
with the backend it finds. If pgvector is unavailable when the job runs, the job
is a graceful no-op (the item stays `pending`; backfill re-embeds once a backend
exists) — it is never dead-lettered for a missing backend. (Note: wiring an
embedding provider *without* pgvector is a misconfiguration — the job will call
the provider and then discard the vector for lack of a store; install pgvector
or leave the embedding provider unset.)

### Embedding freshness contract (D-51)

Async embedding (D-51) deliberately reverses the old synchronous best-effort
embed. The contract:

- **Findable-by-text is immediate.** The `search_vector` `tsvector` is maintained
  by a DB trigger *inside the save transaction*, so full-text search sees an item
  the instant it is saved — never deferred.
- **Findable-by-similarity is eventual.** The semantic (`SemanticSimilarity`)
  index is populated by the embed job, so its freshness is **bounded by the drain
  cadence above** — up to your external-cron interval by default, or the resident
  runner's `poll_interval_secs` for sub-cron latency. Deployments that care about
  embed staleness should enable the resident runner.
- **Not-yet-embedded items degrade cleanly.** Semantic gather does not block on or
  error over an item whose embedding has not landed; it simply is not a
  similarity candidate until its job runs.
- **Coalescing.** Each embed job captures a content hash; if a newer save
  supersedes it before it runs, the stale job is skipped and the newest job
  embeds the latest content exactly once. Delivery is at-least-once, and the
  model-keyed `(item_id, field_name, model)` upsert makes a duplicate run
  idempotent.

### Observable per-item embedding state

`item_embed_status` records each item's embedding lifecycle — `pending`
(enqueued, not yet landed), `indexed` (embedding present for the current model),
or `failed` (the embed job dead-lettered, with the error preserved). This
replaces the old silently-swallowed embed failure. Admin surface:

- `GET  /admin/embed/status` — counts by state.
- `POST /admin/embed/backfill?limit=N` — enqueue embed jobs for up to `N` items
  missing an embedding for the active model (bounded batch). This is also the
  **model-change re-embed** trigger: after the embedding model changes, every
  item is a gap for the new model, so a backfill re-enqueues them. Old-model
  vectors are harmlessly retained (the similarity read path filters by the active
  model). Requires an admin session and `X-CSRF-Token`.

### Policy: async-by-default with per-type opt-out (D-51)

Async is the default for every content type. A type listed in the `embed_policy`
`site_config` key opts out and keeps the synchronous best-effort embed on the
save path:

```sql
-- Make the `page` type embed synchronously on save; everything else stays async.
INSERT INTO site_config (key, value, updated)
VALUES ('embed_policy', '{"sync_types": ["page"]}'::jsonb, NOW())
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated = NOW();
```

An absent or empty `embed_policy` means every type embeds asynchronously.

## Dead-letter queue (DLQ) observability

Dead-lettered jobs are inspectable and recoverable two ways.

**Admin API (admin session + `X-CSRF-Token` header on mutations):**

- `GET  /admin/queue/dlq` — list dead jobs with reason, attempts, and last error.
- `POST /admin/queue/dlq/{id}/requeue` — reset a dead job to `ready` (attempts
  cleared) for another run.
- `POST /admin/queue/dlq/{id}/delete` — discard a dead job.

**SQL / CLI (operators):**

```sql
-- Inspect the dead-letter tier:
SELECT id, plugin_name, queue_name, attempts, max_attempts,
       dead_reason, to_timestamp(dead_at) AS dead_at, last_error
FROM plugin_queue
WHERE status = 'dead'
ORDER BY dead_at DESC;

-- Requeue a specific dead job:
UPDATE plugin_queue
SET status = 'ready', attempts = 0, next_attempt_at = 0, locked_until = 0,
    dead_reason = NULL, dead_at = NULL, last_error = NULL
WHERE id = <id> AND status = 'dead';
```

## Schema (kernel-internal)

`plugin_queue` v2 columns (added additively; existing v1 rows default to a ready,
un-attempted state and survive the migration):

| Column | Meaning |
|--------|---------|
| `attempts` / `max_attempts` | attempts made (incremented at claim) / dead-letter bound |
| `next_attempt_at` | earliest Unix time the job may be claimed (backoff / delay) |
| `priority` | higher drains first |
| `locked_until` | claim lease expiry (reclaimable after it passes) |
| `status` | `ready` \| `claimed` \| `done` (reserved) \| `dead` |
| `last_error` / `dead_reason` / `dead_at` | failure diagnostics for retries and the DLQ |

The schema is internal and not part of the frozen 1.0 contract; the **semantics**
above (at-least-once delivery, honored-bounded concurrency, the `enqueue` surface)
are (D-47, D-48).
