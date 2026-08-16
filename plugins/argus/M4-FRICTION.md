# Argus Milestone 4 — Friction Log

Produced by building **notifications and deployment** as a pure WASM plugin
against the **frozen** PF-5 contract. M1 met the kernel as a pipeline, M2 as an
intelligence engine, M3 as a UI and API consumer; M4 meets it as something that
has to **reach the outside world on a schedule and be shipped to an operator** —
and that is a fourth kernel again. Every item is severity-tagged with `file:line`
evidence and phrased as a concrete, decidable post-1.0 ledger item. **NEW**
findings were surfaced by this build; **RESIDUAL** ones are re-confirmed from
this side. No-friction findings are last, as required.

Verified at `KERNEL_API_VERSION (0,99)`
(`crates/kernel/src/plugin/mod.rs`) with **no kernel, WIT, SDK or
kernel-migration change** in the build session. The design decisions these
findings forced are argued in `M4-DESIGN.md`.

Two findings are load-bearing rather than cosmetic. **G-QUERY-RAW-FIRST-KEYWORD**
is a guard that does not guard what its name says, and M4 depends on it.
**G-SSRF-NO-TEST-ALLOWANCE** is why the end-to-end test asserts a payload and a
refusal rather than a delivery.

This log also records **two defects in Argus's own M3 code** that only showed up
when a fifth migration and a second cron duty were added. Both are fixed here.

---

## Findings

### G-QUERY-RAW-FIRST-KEYWORD — **[Medium, NEW, security]** the read-only guard on `query-raw` is a first-keyword check

`do_query_raw` refuses anything that is not read-only, and `is_read_only` is:

```rust
fn is_read_only(sql: &str) -> bool {
    let first_word = first_sql_keyword(sql);
    first_word.eq_ignore_ascii_case("SELECT") || first_word.eq_ignore_ascii_case("WITH")
}
```

(`crates/kernel/src/host/db.rs:65-68`, applied at `:170`.) Postgres supports
data-modifying CTEs, so

```sql
WITH claimed AS (UPDATE argus_notify_events SET state = 'digested' … RETURNING …)
SELECT … FROM claimed
```

starts with `WITH`, passes the guard, and **writes**. The name `query-raw`, the
function's own doc comment ("Execute a SELECT query"), and the `ERR_DDL_REJECTED`
error code all say otherwise.

**Impact is two-sided, and both sides are worth stating.**

*It is a hole.* A plugin holding `raw_sql` can already write with `execute-raw`,
so this is not privilege escalation — but a reviewer auditing which host calls
mutate will read `query-raw` as read-only and be wrong. The guard exists; it
just does not mean what it appears to mean.

*It is also the only way M4 gets atomicity.* With no transaction (G-DB-NO-TX),
two M4 operations must be one statement or they are races:

- **Claiming a digest** — select the foldable events *and* mark them folded, or
  neither. Two workers folding the same event into two digests would send the
  same story twice.
- **Recording a delivery** — write the outcome *and* return the resulting attempt
  count, so the retry decision is made on a number nothing else can have moved.

Both are `WITH … RETURNING` through `query-raw`
(`plugins/argus/src/notify_ports.rs`, `claim_digest` and `record_delivery`). If
the guard is tightened, these need `execute-raw` plus a separate read — which
reintroduces exactly the race they exist to close — or a transaction surface.

**Recommendation (post-1.0):** decide which it is and say so. Either reject
data-modifying CTEs (`RETURNING` after `INSERT`/`UPDATE`/`DELETE` anywhere in the
statement) and give plugins a scoped transaction instead, **or** rename the pair
to `query`/`execute` and document that `raw_sql` grants read *and* write through
both. The current state — a guard that a plugin author will accidentally or
deliberately step around — is the worst of the three.

### G-QUEUE-NO-INTROSPECTION — **[Medium, NEW]** a plugin cannot ask about its own queue

Queue v2's observability is a database table (`plugin_queue`, with `status`,
`attempts`, `next_attempt_at`, `dead_at`), an admin HTTP surface
(`GET /admin/queue/dlq`, `crates/kernel/src/routes/admin_queue.rs`), and a
documented set of SQL snippets (`docs/plugin-queue.md`, "Dead-letter queue (DLQ)
observability"). There is **no host function**: `crates/plugin-sdk/src/host.rs`
declares `queue_push` and `queue_enqueue` and nothing that reads.

**Impact.** "Alert when the queue stops draining" is a scope requirement and one
of the most valuable alerts Argus has — it is what catches a missing cron poker,
which is the single most likely deployment mistake (see the compose notes in
`README.md`). To answer it, Argus reads `plugin_queue` directly:

```sql
SELECT COALESCE(MAX(CASE WHEN status = 'ready' AND next_attempt_at <= $1 …
FROM plugin_queue WHERE plugin_name = 'argus'
```

(`plugins/argus/src/notify_ports.rs`, `queue_health`.) That is the **only** place
in Argus, across four milestones, that names a table it does not own. It works
only because `raw_sql = true` bypasses the table allowlist by design — the kernel
documents raw SQL as "the auditable escape hatch" that "weakens the table
guarantee for that plugin" (`crates/kernel/src/plugin/db_policy.rs:24-31`). So a
plugin that wants to know whether its own work is draining has to reach for a
capability granted for a different purpose, and a plugin that did *not* declare
`raw_sql` cannot answer the question at all.

**Recommendation (post-1.0, additive):** a `queue-stats(queue_name)` host
function returning `{ready, claimed, dead, oldest_ready_age}` for the calling
plugin's own jobs. It needs no new permission (a plugin already sees everything
it enqueued), it is one query behind the boundary, and it turns a
cross-boundary read into a supported one.

### G-NO-PLUGIN-TIMER — **[Medium, NEW]** "retry with backoff" is not expressible inside a plugin

The scope asks for a Slack channel with "retry + backoff". A WASM plugin has no
`std::thread::sleep` (wasip1 gives no usable sleep on the plugin's thread), no
clock (`host_now()` is a Postgres `EXTRACT(EPOCH FROM NOW())` round trip), and a
150 s epoch deadline that kills a busy-wait. In-process backoff cannot be
written.

**Argus response (a substitution, not a workaround):** every timer is a queue-v2
`delay`. A failed channel re-enqueues itself as a channel-scoped
`Stage::Notify` job with `delay = base * 2^attempt`, capped at an hour
(`crates/argus-core/src/ratelimit.rs`, `retry_delay`;
`crates/argus-core/src/pipeline.rs`, `deliver_and_record`). The observable
behaviour is exactly what was asked for — a transient 5xx is retried with growing
gaps and eventually abandoned — and the delivery row carries the attempt count.

This is the *fourth* mechanism in Argus that is a queue delay wearing a different
hat: the budget pause (M2), the summarize rate limit (M2), the clustering-lease
retry (M2), and now per-channel backoff and quiet-hours deferral (M4). That is
not a complaint about queue v2 — `delay` is excellent and doing a great deal of
work — but it is worth writing down that **the queue is a plugin's only timer**,
because a plugin author who does not realize that will write a sleep loop and
discover the epoch the hard way.

**Recommendation (post-1.0, documentation):** say so in
`docs/plugin-development.md` — "a plugin has no sleep; schedule with
`queue_enqueue(delay)`" — beside the existing epoch note. No code change needed.

### G-ITEM-QUERY-NO-ORDER — **[Low, NEW]** `query-items` promises no ordering

`query-items` takes `type`, `status`, `limit` and `offset`
(`crates/kernel/src/host/item.rs`) and documents no sort. A paged read is
therefore only as stable as Postgres's incidental row order, which is not a
guarantee across an `UPDATE`.

**Impact, and it is small but real.** M3 already sorted feed configurations
explicitly so the round-robin cursor would index a stable list
(`plugins/argus/src/host_ports.rs`, `load_enabled_feeds`, with the reason
written down). M4 hits it again from a new direction: the dispatcher sends to at
most eight channels per job and hands the rest their own jobs, so an unstable
order would silently rotate *which* channels are notified in-job and which are
deferred. `load_enabled_channel_configs` sorts on the channel id for the same
reason (`plugins/argus/src/config_host.rs`).

Two plugins-worth of the same three-line fix suggests the contract should carry
it.

**Recommendation (post-1.0, additive):** accept an `order_by` in the
`query-items` payload, or document that results are ordered by `id` and make it
so. Either removes the trap.

### G-VARIABLES-DOUBLE-NAMESPACE — **[Low, NEW]** a plugin cannot see the key its own variable lives under

The variables host namespaces every key as `plugin.{plugin_name}.{name}`
(`crates/kernel/src/host/variables.rs:48`). Argus additionally names its own keys
`argus.*` so they read sensibly in code, so `argus.notify_threshold` is stored at
`plugin.argus.argus.notify_threshold`. Nothing exposes that mapping to the
plugin, to the admin UI, or to an operator writing `site_config` by hand.

**Impact.** It cost a debugging cycle in this build: the integration tests seeded
`plugin.argus.notify_threshold`, the plugin read
`plugin.argus.argus.notify_threshold`, and the difference was invisible until a
notification silently sat in quiet hours that should have been switched off. An
operator following a configuration table has exactly the same trap in front of
them, and the failure mode is silence rather than an error.

**Recommendation (post-1.0, trivial):** expose the resolved key — a
`variables_key(name) -> String` helper in the SDK, or surface plugin variables in
the admin config UI grouped by plugin. Documenting it is the minimum; Argus's
`README.md` now does, but per-plugin documentation is not a fix.

---

## Residual findings re-confirmed from this side

- **[High → load-bearing, RESIDUAL] G-SSRF-LOCAL, now `G-SSRF-NO-TEST-ALLOWANCE`.**
  M1 filed the loopback block as a **testability nicety**: "a test-only loopback
  allowlist (env-gated) would let consumers integration-test the real host paths
  … Not a freeze change." From M4 it is more than that. The fence blocks loopback
  *and* every RFC-1918 range, at the URL-string layer and again at the resolver
  (`crates/kernel/src/host/http.rs`, `check_url_policy` + `ValidatingResolver`,
  `is_private_ip`), so **there is no address a fixture receiver can bind to that
  the plugin can reach** — not `127.0.0.1`, not the LAN, not the docker bridge.
  The consequence is precise: the transmission half of Argus's headline feature
  cannot be exercised by any automated test on any single machine.

  **What M4 does instead**, and the split is exact:
  - `crates/kernel/tests/argus_notify_test.rs` drives the whole real chain —
    analyze → embed → cluster → summarize → trigger → outbox → dispatcher — and
    asserts the **rendered payload persisted on the delivery row before the
    attempt**, plus the clean per-channel `blocked` outcome. That is real
    pipeline output, not a fixture.
  - `crates/argus-core` proves transmission against an in-memory transport:
    status classification, isolation, retry, blocking, digest, quiet hours.
  - `live_webhook_smoke_run` (`#[ignore]`d, `ARGUS_E2E_WEBHOOK_URL`) is the
    manual run that closes the loop against a real receiver.

  **Recommendation (post-1.0):** the env-gated loopback allowance M1 asked for,
  scoped to the `http` host and off by default. M2 asked for the same thing from
  the AI side. Three milestones have now wanted it.

- **[Medium, RESIDUAL] G-NO-PRESAVE-VETO.** Met a third time, on a third
  configuration type. A notification channel with an unrecognized kind, a target
  that is not a URL, or headers that are not a JSON object cannot be refused —
  only blanked, with the reason written to a note field the administrator may or
  may not read, and the loader then declines to use it
  (`crates/argus-core/src/config.rs`, `coerce_channel`;
  `plugins/argus/src/config_host.rs`, `load_enabled_channel_configs`). The
  failure mode for a notification channel is worse than for a feed: a feed that
  never fetches is visible in the article count, whereas a channel that never
  sends looks exactly like a quiet week.

  One new wrinkle worth recording: the most likely administrator mistake is
  pasting `https://ntfy.sh/mytopic` into the **topic** field. Argus special-cases
  it with a note naming the fix, because a coercion that silently produced
  `https://ntfy.sh/https://ntfy.sh/mytopic` would be worse than useless.

- **[Medium, RESIDUAL] G-DB-NO-TX.** Shaped every write in this milestone. The
  outbox row is written before the dispatch and marked sent after it, so an
  interruption re-dispatches (at-least-once, which the per-channel delivery row
  makes safe) rather than losing the notification. The digest claim and the
  delivery upsert are single statements for the same reason — see
  G-QUERY-RAW-FIRST-KEYWORD for what that costs.

- **[Medium, RESIDUAL] cron cadence is external-only.** M1 predicted a plugin
  with several periodic duties would multiplex them in one `tap_cron`; M4 is the
  fourth duty (feed scheduling, maintenance, spend reporting, alerts). It also
  becomes a **deployment** problem for the first time: nothing in the kernel
  drives cron, so the compose stack ships a `curl` loop container whose only job
  is to POST `/cron/<key>` on an interval. An operator who omits it gets a site
  that comes up and does nothing, with no error anywhere. The stuck-queue alert
  exists partly to catch exactly that — which means the alert that reports the
  problem is itself driven by the thing that is missing, and only fires once
  cron is restored.

- **[Medium, RESIDUAL] G-NO-GATHER-AGGREGATION.** "How many notifications went
  out today" is a pager count on a list gather again
  (`argus_notify_log` in `005_argus_notify.sql`), for the same reason as M3's
  operational tiles.

- **[Medium, RESIDUAL] the only retry signal is a WASM trap.** The notify worker
  never traps on a channel failure, deliberately: a panic would re-send to the
  channels that had already succeeded. Per-channel failure is recorded and
  re-enqueued instead, and the job returns success. The typed
  `succeed | retry{reason} | dead{reason}` outcome would express this directly;
  the workaround is sound and is now used by two stages for two different
  reasons.

- **[Low, RESIDUAL] record admin is read-only.** Correct again: the outbox and
  the delivery rows are exactly what an operator wants to *inspect* and must
  never edit.

---

## No-friction findings (surfaces that just worked)

- **`http-request` for outbound POST.** One-shot `request` returns
  `{status, headers, body}`, which is precisely what a notifier needs to tell
  "retry this" from "the operator's URL is wrong". No streaming, no reassembly,
  no surprises. The `ERR_HTTP_INVALID_URL` code carrying both malformed-URL and
  SSRF-refusal cases is the right granularity for a channel: both are permanent.
- **A fourth Item type cost nothing.** `argus_notify_channel` joined
  `tap_item_info` and the admin content UI, permissions, revisions and gathers
  all worked for it immediately, exactly as M3 found for feeds and topics.
  Reversing nothing, adding one type: about 60 lines of manifest and field
  declarations.
- **`tap_item_presave` and `tap_item_delete` generalized cleanly.** Both taps
  took a third and second type respectively by adding a match arm. The delete tap
  now retires a channel's health row the way it already retired a feed's state
  row.
- **Adding a `Stage` variant was additive everywhere.** One enum arm, one queue
  name, one worker match arm, one `tap_queue_info` entry — and the existing test
  that counted six stages became one that enumerates them from
  `Stage::all()`, so the next stage cannot be added without its queue.
- **Queue-payload evolution was free.** `JobPayload` gained an optional
  `channel` field with `#[serde(default, skip_serializing_if)]`, so jobs already
  sitting in a live queue deserialize unchanged and newly written payloads are
  byte-identical when the field is absent. Pinned by a test.
- **CI needed no change.** The integration-test shards enumerate targets from
  `cargo metadata` rather than a hard-coded list
  (`.github/workflows/ci.yml`, "Run integration tests"), so
  `argus_notify_test` was picked up and assigned to a shard automatically. The
  comment there explains that this exists precisely so a new test file cannot be
  silently unrun — and it paid off in this build.
- **The host-agnostic core split paid for itself a fourth time.** Three new pure
  modules (`notify`, `ratelimit`, `judge`) with 354 native unit tests in
  `argus-core`, all fast and DB-free. Every quiet-hours boundary, every
  midnight crossing, every digest threshold, every retry delay and every
  status-classification rule is tested with no host in sight — including a
  property test that walks all 480 minutes of the default quiet window and
  asserts each one defers forward and lands outside it.

---

## Defects found in Argus's own M3 code

Not kernel friction. Both were invisible until M4 added a fifth migration and a
second cron duty, and both are fixed in this commit.

1. **`004_argus_reader.sql` was never registered.** M3 wrote the migration and
   left it out of the manifest's `[migrations] files` list
   (`plugins/argus/argus.info.toml`), so on a **real install** the whole M3
   reader surface — `argus_reactions`, `argus_read_state`, `argus_subscriptions`,
   every gather, every URL alias, the two roles and the three tiles — never
   existed. The M3 tests applied it by hand and so never noticed. Fixed by adding
   `004` and `005` to the list.

   Worth generalizing: `[migrations] files` is a hand-maintained list beside a
   directory of files, and nothing checks that the two agree. A kernel-side
   warning for a `.sql` file in `migrations/` that no manifest names would have
   caught this at plugin load.

2. **The legacy-config backfill choked on its own state rows.** From M3 on,
   `argus_feeds` is a state-only table whose rows are created on demand by the
   first fetch with every configuration column `NULL`. `backfill_legacy_config`
   selected `url`, `name` and `topic_id` into a row type where all three are
   `String`, so the moment any feed had fetched once the backfill failed to
   decode — `invalid type: null, expected a string` — and, because the
   completion marker is written only on success, it **failed again on every cron
   tick forever**. Observed in this build's integration output. Fixed with a
   `WHERE url IS NOT NULL AND name IS NOT NULL AND topic_id IS NOT NULL` filter,
   which is what distinguishes a legacy configuration row from a state row, and
   pinned by an assertion in `argus_notify_test.rs` that `tap_cron`'s
   `config_backfill` result carries no error.

---

## Deviations from the M4 scope list (with reasoning)

1. **Notification channels are Items, not a JSON site variable.** Forced by
   G-NO-PLUGIN-HTTP and the read-only record admin, and consistent with M3
   Decision 1. `M4-DESIGN.md` Decision 1.
2. **Retry with backoff is a delayed re-enqueue, not an in-process loop** —
   forced by G-NO-PLUGIN-TIMER. `M4-DESIGN.md` Decision 4.
3. **Quiet hours are a configured UTC offset, not a timezone** — forced by the
   absence of a clock and a tz database in wasm. An operator in a DST zone
   adjusts twice a year. `M4-DESIGN.md` Decision 5.
4. **The end-to-end test asserts the rendered payload and a clean refusal, not a
   delivery** — forced by G-SSRF-NO-TEST-ALLOWANCE. The transmission half is
   proved in `argus-core` against an in-memory transport, and by a manual
   `#[ignore]`d run against a real receiver. `M4-DESIGN.md` Decision 10.
5. **The e2e drives the pipeline from *decided articles*, not from a fixture RSS
   server.** The scope asked for "seed feeds pointing at a fixture HTTP server
   serving RSS (respecting the SSRF-fence testing pattern)". Respecting that
   pattern means not doing it: a local RSS server is as unreachable as a local
   webhook receiver. This is the same substitution M2's chain test made, for the
   same reason. Feed fetch against real feeds is covered by M1's and M2's smoke
   runs.
6. **The queue-stuck alert reads a kernel table.** Forced by
   G-QUEUE-NO-INTROSPECTION. It is the only such read in the plugin and it is
   read-only and narrow.
7. **The digest is built at send time from pending events, not by a windowing
   scheduler.** There is no scheduler to run a window job on and no clock to
   time one with; the collapse happens when the first qualifying event comes due.
   `M4-DESIGN.md` Decision 6.
8. **The story-update judge is gated by the daily budget; decide still is not.**
   M2 excluded decide from gating because pausing it would *raise* the bill. That
   argument does not apply to the judge — a missed update notification costs a
   reader one message — so it is gated like analyze and summarize.
9. **APNS is declared out of scope and no stub ships.** `ChannelKind` has three
   variants, not four. A fourth variant that returned "not implemented" would be
   a promise in the admin UI that nothing keeps.
10. **The compose `postgres` service moved to `pgvector/pgvector:pg17`.** Not in
    the scope list as a change to the *shared* service, but the scope asked for
    "Postgres with pgvector" and adding a second Postgres service beside the
    existing one would have been worse. Same server, same data layout, one extra
    extension available; the kernel's conditional embedding migration is what
    uses it. Argus itself still uses lexical vectors in `TEXT` (M2 Decision 1),
    so nothing in Argus depends on it. CI is unaffected — it runs `postgres:16`
    service containers, not compose.
11. **M2 deviation 8 is still not fixed.** An unparseable decide response still
    loses its cost. M3 deferred it as a pipeline change in a UI commit; M4 defers
    it as a pipeline change in a notification commit. It is now three milestones
    old and should be picked up on its own.

---

## What runs, and what it cost

### The demo narrative

A fixture feed spike, end to end, as the `argus-core` and integration tests
exercise it:

1. Five articles on one topic clear the relevance floor within a fifteen-minute
   window and each founds or joins a story.
2. Each summarize records a `story.new` event in the outbox and enqueues a
   dispatch. The first one to run counts the other four as pending, due and
   digestible, claims them in one statement, and rewrites itself as a digest
   titled **"5 new stories"** listing each headline.
3. The four folded events are marked `digested` and never send. One message goes
   out where five would have.
4. Run the same spike at 01:00 with the default quiet window and nothing goes
   out: each event's `scheduled_at` is pushed to 07:00 and its job re-enqueued
   with exactly that delay. At 07:00 the digest fires.
5. Put one of those stories on a **high-priority topic** and it bypasses all of
   it — no debounce, no quiet hours, no digest — and arrives at 01:00 as an ntfy
   notification at priority 4.
6. Take a feed offline for three fetches and `tap_cron` records an
   `alert.feed_failing` naming the feed and its last error. Stop the cron poker
   and the next tick (once cron returns) records `alert.queue_stuck` at high
   priority, which bypasses quiet hours by default.

### Test counts

| Suite | Count | Notes |
|---|---|---|
| `argus-core` unit tests | **354** | fast, DB-free, no host |
| `argus` plugin unit tests | **46** | tap contracts and payload compatibility |
| `argus_notify_test` (integration) | **7** + 1 `#[ignore]` | real wasm, real queue drain, ~29 s |

### Compose

The `argus` profile brings up Postgres (pgvector), Redis, Trovato with the plugin
installed and enabled, and the cron poker. `plugin install` runs the kernel
migrations, then argus's five, then enables the plugin, and only then does the
server start; it is idempotent, so a restart is a no-op and an upgrade applies
whatever is new.

**Measured, on an Apple silicon laptop with Docker 29.4:**

| Step | Time |
|---|---|
| `docker compose --profile argus build argus` (cold Rust build in-container) | **226 s** |
| `docker compose --profile argus up -d` → `argus` container healthy | **31 s** |
| First `tap_cron` for argus after that | inside the first 60 s cron tick |

The health check passes only after `plugin install` has run the kernel
migrations, argus's five, and enabled the plugin, so 31 s is the whole
cold-start-to-serving figure on an already-built image.

**"Time to first story" is deliberately not quoted.** It is dominated by things
that are not Argus: how often the feed you chose publishes, what fetch interval
you set (300 s minimum), and how fast your AI provider answers. A number here
would be a number that measured a feed rather than a pipeline. What *is*
measured about the pipeline itself is M2's live smoke run — 345 jobs drained in
31 s, 71 articles into 38 stories.

**One real finding from bringing the stack up:** the Postgres image change is not
free on an *existing* volume. `pgvector/pgvector:pg17` and `postgres:17` are
built on different bases, so Postgres reports `database "trovato" has a collation
version mismatch` (glibc 2.41 → 2.36 in the observed case) and text index
ordering could differ from what those indexes were built with. Not a defect in
either image, and harmless on a fresh volume, but it is exactly the kind of thing
that is discovered in production if nobody writes it down. The compose file now
takes `POSTGRES_IMAGE` so an existing deployment can pin the old image, and both
it and `.env.example` carry the `REINDEX` + `REFRESH COLLATION VERSION` repair.

### Remaining gaps before this is sellable

Honest list, in the order they would bite a first customer:

1. **Semantic clustering.** Stories still cluster on lexical vectors because
   `ai-request` will not serve an embedding (G-AI-EMBED-UNROUTED). Two outlets
   reporting one event in different words do not cluster unless they share
   entities. This is *the* product-quality gap and it is a kernel fix.
2. **Stories are not semantically searchable.** A plugin-created Item is never
   embedded (G-ITEM-NO-EMBED), so the reason stories are Items does not pay off
   without a manual admin backfill.
3. **No reader write path.** Upvotes, bookmarks and topic subscriptions have
   schemas, storage functions and tests, and no caller (G-NO-PLUGIN-HTTP). A
   reader can read and comment; they cannot react.
4. **Delegated administration does not work through the UI.** The `argus_admin`
   role grants everything it should, and `/admin/content/...` checks
   `users.is_admin` rather than permissions (G-ADMIN-UI-IS-ADMIN-ONLY), so feeds
   are manageable only by a superuser or through the JSON routes.
5. **The join threshold is uncalibrated.** 0.55 was chosen against lexical
   vectors and a fixture analyzer. It needs a real analyzer and a real feed set
   before stories go in front of a reader.
6. **No delivery receipts beyond HTTP status.** A 2xx from ntfy means ntfy took
   it, not that a phone showed it. Adequate, and worth saying out loud.
